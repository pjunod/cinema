//! Closed sharing management and peer surfaces. Never mount peers on the local API.
use super::{error::ApiError, extract::AdminUser};
use crate::state::{clock_ms, AppState};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, Request, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use plurx_core::{error::StoreError, secrets::Secret, sharing::*, store::keys};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::{sync::LazyLock, time::Duration};
use uuid::Uuid;

const BODY_LIMIT: usize = 16 * 1024;
pub(crate) async fn local_peer_refusal() -> ApiError {
    failure(StatusCode::NOT_FOUND, "sharing_not_found")
}
static PEER_REQUESTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(32));
fn failure(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::typed(status, code, code.replace('_', " "))
}
fn invalid() -> ApiError {
    failure(StatusCode::BAD_REQUEST, "sharing_invalid_request")
}
fn authority(_: StoreError) -> ApiError {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_authority_unavailable",
    )
}
async fn body<T: DeserializeOwned>(body: Body) -> Result<T, ApiError> {
    let bytes = to_bytes(body, BODY_LIMIT).await.map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
fn id(value: &str) -> Result<Uuid, ApiError> {
    let id = Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.to_string() != value {
        return Err(invalid());
    }
    Ok(id)
}
fn query_id(query: Option<&str>, field: &str) -> Result<Option<Uuid>, ApiError> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Ok(None);
    };
    if query.len() > 128 {
        return Err(invalid());
    }
    let (key, value) = query.split_once('=').ok_or_else(invalid)?;
    if key != field {
        return Err(invalid());
    }
    id(value).map(Some)
}
pub(super) fn credential(headers: &HeaderMap) -> Result<Secret, ApiError> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("CinemaShare "))
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "sharing_peer_auth_failed"))?;
    if values.next().is_some() {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "sharing_peer_auth_failed",
        ));
    }
    let secret = Secret::from_cleartext(value);
    validate_secret(&secret)
        .map_err(|_| failure(StatusCode::UNAUTHORIZED, "sharing_peer_auth_failed"))?;
    Ok(secret)
}
fn mutation(result: MutationOutcome) -> Result<Json<Value>, ApiError> {
    match result {
        MutationOutcome::Capacity => {
            Err(failure(StatusCode::TOO_MANY_REQUESTS, "sharing_capacity"))
        }
        MutationOutcome::Applied => Ok(Json(json!({"updated":true}))),
        MutationOutcome::Conflict => {
            Err(failure(StatusCode::CONFLICT, "sharing_generation_conflict"))
        }
        MutationOutcome::NotFound => Err(failure(StatusCode::NOT_FOUND, "sharing_not_found")),
        MutationOutcome::Expired => Err(failure(StatusCode::GONE, "sharing_expired")),
    }
}
pub(crate) fn admin_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/sharing/settings", get(settings).put(save_settings))
        .route("/sharing/status", get(status))
        .route("/sharing/endpoints", get(manifest).put(save_manifest))
        .route("/sharing/invitations", post(invite))
        .route("/sharing/invitations/{id}", delete(cancel_invite))
        .route("/sharing/exports", get(exports))
        .route("/sharing/exports/{id}/approve", post(approve))
        .route("/sharing/exports/{id}/libraries", put(scope))
        .route("/sharing/exports/{id}", delete(revoke))
        .route("/sharing/imports", get(imports).post(create_import))
        .route("/sharing/imports/{id}/re-pair", post(re_pair))
        .route("/sharing/imports/{id}/rotate", post(rotate_import))
        .route(
            "/sharing/imports/{id}/assignments",
            get(assignment_snapshot).put(assignments),
        )
        .route("/sharing/imports/{id}/endpoints", put(import_endpoints))
        .route("/sharing/imports/{id}", delete(disconnect))
        .merge(super::shared_library::admin_library_router(state))
        .layer(axum::middleware::from_fn(private_response))
}
pub(crate) fn peer_router(state: AppState) -> Router {
    Router::new()
        .route("/sharing/v1/identity", get(identity))
        .route("/sharing/v1/claims", post(claim))
        .route("/sharing/v1/grant", get(grant))
        .route("/sharing/v1/endpoints", get(peer_manifest))
        .route("/sharing/v1/grant/rotation", post(rotate))
        .route("/sharing/v1/grant/rotation/{id}", get(rotation_status))
        .merge(super::shared_library::peer_router(state.clone()))
        .merge(super::shared_playback::peer_router(state.clone()))
        .merge(super::shared_source_playback::peer_router(state.clone()))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            peer_guard,
        ))
        .fallback(peer_fallback)
        .layer(axum::middleware::from_fn(private_response))
        .with_state(state)
}
async fn private_response(mut request: Request<Body>, next: Next) -> Response {
    if let Some(value) = request.headers_mut().get_mut(header::AUTHORIZATION) {
        value.set_sensitive(true);
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn peer_fallback(uri: Uri) -> ApiError {
    if uri.path().strip_prefix("/sharing/").is_some_and(|p| {
        p.split('/')
            .next()
            .is_some_and(|v| v.starts_with('v') && v != "v1")
    }) {
        failure(StatusCode::UPGRADE_REQUIRED, "sharing_protocol_unsupported")
    } else {
        failure(StatusCode::NOT_FOUND, "sharing_not_found")
    }
}
async fn peer_guard(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(value) = request.headers_mut().get_mut(header::AUTHORIZATION) {
        value.set_sensitive(true);
    }
    if let Some(value) = request.headers_mut().get_mut("cinemashare-viewer") {
        value.set_sensitive(true);
    }
    let _permit = match PEER_REQUESTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return failure(StatusCode::TOO_MANY_REQUESTS, "sharing_capacity").into_response()
        }
    };
    match crate::sharing::enabled(state.store.as_ref()).await {
        Ok(true) => {}
        Ok(false) => {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "sharing_disabled").into_response()
        }
        Err(error) => return authority(error).into_response(),
    }
    let deadline = match request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|path| path.as_str())
    {
        Some("/sharing/v1/items/{item}/files/{file}/decision") => Duration::from_secs(10),
        Some("/sharing/v1/items/{item}/files/{file}/sessions") => Duration::from_secs(310),
        _ => Duration::from_secs(3),
    };
    match tokio::time::timeout(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_authority_unavailable",
        )
        .into_response(),
    }
}
async fn name(state: &AppState) -> Result<String, ApiError> {
    let name = state
        .store
        .get_setting(keys::SERVER_NAME)
        .await
        .map_err(authority)?
        .unwrap_or_else(|| "Cinema".into());
    if name.len() > 128 || name.chars().any(char::is_control) {
        return Ok("Cinema".into());
    }
    Ok(name)
}
async fn identity(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let source = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(authority)?;
    Ok(Json(
        json!({"server_id":source.server_id,"catalogue_epoch":source.catalogue_epoch,"name":name(&state).await?,"protocol_min":1,"protocol_max":1}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRequest {
    #[serde(deserialize_with = "canonical_uuid")]
    invitation_id: Uuid,
    #[serde(deserialize_with = "wire_secret")]
    invitation_secret: Secret,
    #[serde(deserialize_with = "canonical_uuid")]
    claim_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    recipient_server_id: Uuid,
    recipient_name: String,
    #[serde(deserialize_with = "wire_secret")]
    grant_credential: Secret,
}
async fn claim(State(state): State<AppState>, raw: Body) -> Result<Json<Value>, ApiError> {
    let c: ClaimRequest = body(raw).await?;
    if c.recipient_name.len() > 128 || c.recipient_name.chars().any(char::is_control) {
        return Err(invalid());
    }
    let result = state
        .store
        .claim_share(ShareClaim {
            invitation_id: c.invitation_id,
            invitation_hash: secret_hash(SecretDomain::Invitation, &c.invitation_secret),
            claim_id: c.claim_id,
            grant_id: Uuid::new_v4(),
            recipient_server_id: c.recipient_server_id,
            recipient_name: c.recipient_name,
            credential_hash: secret_hash(SecretDomain::Grant, &c.grant_credential),
            now_ms: clock_ms(),
        })
        .await
        .map_err(authority)?;
    match result {
        ClaimOutcome::Created(g) | ClaimOutcome::Replay(g) => Ok(Json(
            json!({"grant_id":g.id,"state":g.state,"pending_expires_at_ms":g.pending_expires_at_ms,"protocol_min":1,"protocol_max":1}),
        )),
        ClaimOutcome::Consumed => Err(failure(StatusCode::CONFLICT, "invitation_consumed")),
        ClaimOutcome::Expired => Err(failure(StatusCode::GONE, "sharing_expired")),
        ClaimOutcome::Cancelled | ClaimOutcome::NotFound => {
            Err(failure(StatusCode::NOT_FOUND, "sharing_not_found"))
        }
        ClaimOutcome::Capacity => Err(failure(StatusCode::TOO_MANY_REQUESTS, "sharing_capacity")),
    }
}
async fn own_grant(state: &AppState, headers: &HeaderMap) -> Result<ExportSummary, ApiError> {
    let secret = credential(headers)?;
    state
        .store
        .sharing_grant_status(&secret_hash(SecretDomain::Grant, &secret))
        .await
        .map_err(authority)?
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "sharing_peer_auth_failed"))
}
async fn grant(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let summary = own_grant(&state, &headers).await?;
    let expired = summary.grant.state == GrantState::Pending
        && summary.grant.pending_expires_at_ms <= clock_ms();
    let mut value = serde_json::to_value(&summary.grant).map_err(|_| invalid())?;
    if expired {
        value["state"] = json!("expired");
    }
    value["pairing_code"] = json!(summary.pairing_code);
    value["protocol_min"] = json!(1);
    value["protocol_max"] = json!(1);
    // Media capabilities are advertised only once S4/S5 implement them.
    value["supported_media"] = json!([]);
    Ok(Json(value))
}
async fn peer_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let secret = credential(&headers)?;
    if !matches!(
        state
            .store
            .authorize_share(&secret_hash(SecretDomain::Grant, &secret), None, clock_ms())
            .await
            .map_err(authority)?,
        Authorization::Allowed(_)
    ) {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "sharing_peer_auth_failed",
        ));
    }
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(authority)?;
    let manifest = state
        .store
        .sharing_endpoint_manifest()
        .await
        .map_err(authority)?
        .ok_or_else(|| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_endpoints_unavailable",
            )
        })?;
    Ok(Json(
        json!({"server_id":identity.server_id,"catalogue_epoch":identity.catalogue_epoch,"revision":manifest.revision,"endpoints":manifest.endpoints}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationRequest {
    #[serde(deserialize_with = "canonical_uuid")]
    request_id: Uuid,
    #[serde(deserialize_with = "wire_secret")]
    grant_credential: Secret,
}
async fn rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let current = credential(&headers)?;
    let hash = secret_hash(SecretDomain::Grant, &current);
    let grant = match state
        .store
        .authorize_share(&hash, None, clock_ms())
        .await
        .map_err(authority)?
    {
        Authorization::Allowed(g) => g,
        _ => {
            return Err(failure(
                StatusCode::UNAUTHORIZED,
                "sharing_peer_auth_failed",
            ))
        }
    };
    let r: RotationRequest = body(raw).await?;
    mutation(
        state
            .store
            .rotate_share(
                grant.id,
                r.request_id,
                &hash,
                &secret_hash(SecretDomain::Grant, &r.grant_credential),
                clock_ms(),
            )
            .await
            .map_err(authority)?,
    )
}
async fn rotation_status(
    State(state): State<AppState>,
    Path(request): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let secret = credential(&headers)?;
    let confirmed = state
        .store
        .share_rotation_status(
            query_id(query.as_deref(), "grant_id")?.ok_or_else(invalid)?,
            id(&request)?,
            &secret_hash(SecretDomain::Grant, &secret),
            clock_ms(),
        )
        .await
        .map_err(authority)?;
    if !confirmed {
        return Err(failure(StatusCode::NOT_FOUND, "sharing_rotation_not_found"));
    }
    Ok(Json(json!({"confirmed":true})))
}
async fn settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"enabled":crate::sharing::enabled(state.store.as_ref()).await.map_err(authority)?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsRequest {
    enabled: bool,
}
async fn save_settings(
    _admin: AdminUser,
    State(state): State<AppState>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let update: SettingsRequest = body(raw).await?;
    state
        .store
        .put_setting(
            keys::SHARING_ENABLED,
            if update.enabled { "1" } else { "0" },
        )
        .await
        .map_err(authority)?;
    if !update.enabled {
        state.sharing.disable_bodies();
    }
    Ok(Json(json!({"enabled":update.enabled})))
}
async fn status(_admin: AdminUser, State(state): State<AppState>) -> Json<Value> {
    // Transport state belongs to this process; a healthy peer is not evidence
    // that another cluster member has a key, listener or qualified route.
    let mut status = serde_json::to_value(state.sharing.status())
        .expect("sharing status contains only JSON-compatible fields");
    let object = status.as_object_mut().expect("sharing status is an object");
    object.insert("node_id".into(), json!(state.node_id));
    object.insert("observed_at_ms".into(), json!(clock_ms()));
    object.insert("observation_scope".into(), json!("local_node"));
    Json(status)
}
async fn manifest(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"manifest":state.store.sharing_endpoint_manifest().await.map_err(authority)?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRequest {
    expected_revision: i64,
    endpoints: Vec<Endpoint>,
}
async fn save_manifest(
    _admin: AdminUser,
    State(state): State<AppState>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let update: ManifestRequest = body(raw).await?;
    validate_endpoints(&update.endpoints).map_err(|_| invalid())?;
    if update.expected_revision < 0 {
        return Err(invalid());
    }
    mutation(
        state
            .store
            .set_sharing_endpoint_manifest(update.expected_revision, update.endpoints)
            .await
            .map_err(authority)?,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationRequest {
    library_ids: Vec<SourceId>,
    ttl_seconds: Option<i64>,
}
fn library_ids(ids: Vec<SourceId>) -> Result<Vec<i64>, ApiError> {
    if ids.len() > MAX_LIBRARIES {
        return Err(invalid());
    }
    let ids: Vec<i64> = ids
        .into_iter()
        .map(|id| id.as_str().parse().map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    let distinct: std::collections::HashSet<_> = ids.iter().copied().collect();
    if ids.iter().any(|id| *id < 0) || distinct.len() != ids.len() {
        return Err(invalid());
    }
    Ok(ids)
}
async fn invite(
    _admin: AdminUser,
    State(state): State<AppState>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let request: InvitationRequest = body(raw).await?;
    let ttl = request.ttl_seconds.unwrap_or(86400);
    if !(1..=MAX_INVITATION_TTL_MS / 1000).contains(&ttl) {
        return Err(invalid());
    }
    let libraries = library_ids(request.library_ids)?;
    if libraries.is_empty() {
        return Err(invalid());
    }
    for id in &libraries {
        let library = state
            .store
            .get_library(*id)
            .await
            .map_err(authority)?
            .ok_or_else(invalid)?;
        if !matches!(
            library.kind,
            plurx_core::domain::LibraryKind::Movies | plurx_core::domain::LibraryKind::Shows
        ) {
            return Err(invalid());
        }
    }
    let now = clock_ms();
    let secret = new_secret().map_err(authority)?;
    let identity = state.store.sharing_identity(now).await.map_err(authority)?;
    let endpoints = state
        .store
        .sharing_endpoint_manifest()
        .await
        .map_err(authority)?
        .ok_or_else(|| failure(StatusCode::CONFLICT, "sharing_endpoints_unavailable"))?
        .endpoints;
    let invite = Invitation {
        identity,
        name: name(&state).await?,
        endpoints,
        id: Uuid::new_v4(),
        secret,
        expires_at_ms: now.checked_add(ttl * 1000).ok_or_else(invalid)?,
    };
    let blob = invite.encode().map_err(|_| invalid())?;
    let _ = mutation(
        state
            .store
            .create_share_invitation(InvitationRecord {
                id: invite.id,
                token_hash: secret_hash(SecretDomain::Invitation, &invite.secret),
                library_ids: libraries,
                created_at_ms: now,
                expires_at_ms: invite.expires_at_ms,
            })
            .await
            .map_err(authority)?,
    )?;
    Ok(Json(
        json!({"id":invite.id,"invitation":blob.expose(),"expires_at_ms":invite.expires_at_ms}),
    ))
}
async fn cancel_invite(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .cancel_share_invitation(id(&value)?)
        .await
        .map_err(authority)?;
    Ok(Json(json!({"cancelled":true})))
}
async fn exports(
    _admin: AdminUser,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let after = query_id(query.as_deref(), "after")?;
    let mut exports = state
        .store
        .sharing_exports(after)
        .await
        .map_err(authority)?;
    let next = if exports.len() > 32 {
        exports.truncate(32);
        exports.last().map(|e| e.grant.id)
    } else {
        None
    };
    Ok(Json(json!({"exports":exports,"next":next})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRequest {
    expected_mutation_generation: i64,
    pairing_code: String,
}
async fn approve(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let grant_id = id(&value)?;
    let request: ApprovalRequest = body(raw).await?;
    if request.expected_mutation_generation < 1 || request.pairing_code.len() != 16 {
        return Err(invalid());
    }
    let previous = Uuid::from_u128(grant_id.as_u128().checked_sub(1).ok_or_else(invalid)?);
    let grant = state
        .store
        .sharing_exports(Some(previous))
        .await
        .map_err(authority)?
        .into_iter()
        .find(|e| e.grant.id == grant_id)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sharing_not_found"))?;
    if request.pairing_code != grant.pairing_code {
        return Err(failure(
            StatusCode::CONFLICT,
            "sharing_pairing_code_mismatch",
        ));
    }
    mutation(
        state
            .store
            .approve_share(grant_id, request.expected_mutation_generation, clock_ms())
            .await
            .map_err(authority)?,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeRequest {
    expected_mutation_generation: i64,
    library_ids: Vec<SourceId>,
}
async fn scope(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let request: ScopeRequest = body(raw).await?;
    if request.expected_mutation_generation < 1 {
        return Err(invalid());
    }
    mutation(
        state
            .store
            .share_scope(
                id(&value)?,
                request.expected_mutation_generation,
                library_ids(request.library_ids)?,
                clock_ms(),
            )
            .await
            .map_err(authority)?,
    )
}
async fn revoke(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .revoke_share(id(&value)?, clock_ms())
        .await
        .map_err(authority)?;
    Ok(Json(json!({"revoked":true})))
}
async fn imports(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let summaries = state.store.sharing_imports().await.map_err(authority)?;
    let mut imports = Vec::with_capacity(summaries.len());
    for summary in summaries {
        imports.push(import_status(&state, summary.id).await?);
    }
    Ok(Json(json!({"imports":imports})))
}
async fn assignment_snapshot(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<ImportAssignmentSnapshot>, ApiError> {
    state
        .store
        .sharing_import_assignments(id(&value)?)
        .await
        .map_err(authority)?
        .map(Json)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sharing_not_found"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentGroup {
    library_id: SourceId,
    user_ids: Vec<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentsRequest {
    expected_assignment_generation: i64,
    assignments: Vec<AssignmentGroup>,
}
async fn assignments(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let request: AssignmentsRequest = body(raw).await?;
    if request.expected_assignment_generation < 1
        || request.assignments.len() > MAX_LIBRARIES
        || request
            .assignments
            .iter()
            .any(|group| group.user_ids.len() > 256)
    {
        return Err(invalid());
    }
    let assignments = request
        .assignments
        .into_iter()
        .flat_map(|group| {
            group.user_ids.into_iter().map(move |user_id| Assignment {
                library_id: group.library_id.clone(),
                user_id,
            })
        })
        .collect();
    mutation(
        state
            .store
            .assign_share_viewers(
                id(&value)?,
                request.expected_assignment_generation,
                assignments,
                clock_ms(),
            )
            .await
            .map_err(authority)?,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportEndpointsRequest {
    expected_endpoint_generation: i64,
    endpoints: Vec<Endpoint>,
    #[serde(default)]
    confirm_new_pins: bool,
}
async fn import_endpoints(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let request: ImportEndpointsRequest = body(raw).await?;
    if request.expected_endpoint_generation < 1 {
        return Err(invalid());
    }
    validate_endpoints(&request.endpoints).map_err(|_| invalid())?;
    let import_id = id(&value)?;
    let import = state
        .store
        .sharing_import(import_id)
        .await
        .map_err(authority)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sharing_not_found"))?;
    if !request.confirm_new_pins
        && request.endpoints.iter().any(|new| {
            !import
                .summary
                .endpoints
                .iter()
                .any(|old| old.spki_sha256 == new.spki_sha256)
        })
    {
        return Err(failure(
            StatusCode::CONFLICT,
            "sharing_pin_confirmation_required",
        ));
    }
    mutation(
        state
            .store
            .set_sharing_import_endpoints(
                import_id,
                request.expected_endpoint_generation,
                request.endpoints,
                None,
                clock_ms(),
            )
            .await
            .map_err(authority)?,
    )
}
async fn disconnect(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .store
        .disable_share_import(id(&value)?, clock_ms())
        .await
        .map_err(authority)?;
    Ok(Json(json!({"disabled":true})))
}

fn peer_error(error: crate::sharing_client::PeerError) -> ApiError {
    use crate::sharing_client::PeerError;
    match error {
        PeerError::IdentityMismatch => {
            failure(StatusCode::BAD_GATEWAY, "sharing_identity_mismatch")
        }
        PeerError::ProtocolUnsupported => {
            failure(StatusCode::BAD_GATEWAY, "sharing_protocol_unsupported")
        }
        PeerError::Authentication => failure(StatusCode::BAD_GATEWAY, "sharing_peer_auth_failed"),
        PeerError::InvalidResponse => {
            failure(StatusCode::BAD_GATEWAY, "sharing_invalid_peer_response")
        }
        PeerError::Unavailable | PeerError::Rejected(_) => {
            failure(StatusCode::BAD_GATEWAY, "sharing_peer_unavailable")
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    #[serde(deserialize_with = "invitation_secret")]
    invitation: Secret,
    endpoint_overrides: Option<Vec<Endpoint>>,
}
fn invitation_secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Secret, D::Error> {
    String::deserialize(deserializer).map(Secret::from_cleartext)
}
async fn verified_import(
    state: &AppState,
    request: ImportRequest,
    import_id: Uuid,
) -> Result<NewImport, ApiError> {
    use plurx_core::secrets::SharingSecretPurpose;
    if !crate::sharing::enabled(state.store.as_ref())
        .await
        .map_err(authority)?
    {
        return Err(failure(StatusCode::SERVICE_UNAVAILABLE, "sharing_disabled"));
    }
    let mut invitation = Invitation::parse(&request.invitation).map_err(|_| invalid())?;
    let now = clock_ms();
    if invitation.expires_at_ms <= now {
        return Err(failure(StatusCode::GONE, "sharing_expired"));
    }
    // The source issues invitations for at most seven days. Reject impossible
    // deadlines before DNS or TLS, with a small allowance for clock skew.
    if invitation.expires_at_ms > now.saturating_add(7 * 86400 * 1000 + 5 * 60 * 1000) {
        return Err(invalid());
    }
    let claim_deadline_ms = invitation
        .expires_at_ms
        .checked_add(PENDING_TTL_MS)
        .ok_or_else(invalid)?;
    if let Some(endpoints) = request.endpoint_overrides {
        validate_endpoints(&endpoints).map_err(|_| invalid())?;
        if endpoints.iter().any(|new| {
            !invitation
                .endpoints
                .iter()
                .any(|old| old.spki_sha256 == new.spki_sha256)
        }) {
            return Err(failure(
                StatusCode::CONFLICT,
                "sharing_pin_confirmation_required",
            ));
        }
        invitation.endpoints = endpoints;
    }
    let (_peer, source) = crate::sharing_client::PeerConnection::verified(
        &state.sharing,
        &invitation.endpoints,
        &invitation.identity,
    )
    .await
    .map_err(peer_error)?;
    let local = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(authority)?;
    if local.server_id == source.server_id {
        return Err(failure(StatusCode::CONFLICT, "sharing_self_import"));
    }
    let started = clock_ms();
    let credential = new_secret().map_err(authority)?;
    let credential = crate::sharing::ImportCredential::encode(
        &credential,
        invitation.id,
        started,
        &name(state).await?,
        claim_deadline_ms,
        None,
    )
    .map_err(authority)?;
    let blob = invitation.encode().map_err(|_| invalid())?;
    let credential = state
        .sharing
        .key
        .seal_sharing(
            SharingSecretPurpose::Credential,
            local.server_id,
            import_id,
            credential.expose(),
        )
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_credential_unavailable",
            )
        })?;
    let claim_secret = state
        .sharing
        .key
        .seal_sharing(
            SharingSecretPurpose::Claim,
            local.server_id,
            import_id,
            blob.expose(),
        )
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_credential_unavailable",
            )
        })?;
    Ok(NewImport {
        id: import_id,
        source: invitation.identity,
        source_name: source.name,
        claim_id: Uuid::new_v4(),
        credential,
        claim_secret,
        endpoints: invitation.endpoints,
        now_ms: started,
    })
}
async fn import_status(state: &AppState, import_id: Uuid) -> Result<Value, ApiError> {
    let import = state
        .store
        .sharing_import(import_id)
        .await
        .map_err(authority)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sharing_not_found"))?;
    let local = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(authority)?;
    let credentials =
        crate::sharing::ImportCredential::open(&state.sharing, local.server_id, &import)
            .map_err(authority)?;
    let code = pairing_code(
        import.summary.source_server_id,
        local.server_id,
        credentials.invitation_id,
        import.summary.claim_id,
        &secret_hash(SecretDomain::Grant, &credentials.credential),
    );
    Ok(json!({"import":import.summary,"pairing_code":code}))
}
async fn create_import(
    _admin: AdminUser,
    State(state): State<AppState>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let import_id = Uuid::new_v4();
    let import = verified_import(&state, body(raw).await?, import_id).await?;
    match state
        .store
        .create_share_import(import)
        .await
        .map_err(authority)?
    {
        ImportOutcome::Capacity => Err(failure(StatusCode::TOO_MANY_REQUESTS, "sharing_capacity")),
        ImportOutcome::AlreadyImported(existing) => Err(ApiError::typed_detail(
            StatusCode::CONFLICT,
            "already_imported",
            "Source is already imported",
            json!({"import_id":existing}),
        )),
        ImportOutcome::Created => {
            if let Some(import) = state
                .store
                .sharing_import(import_id)
                .await
                .map_err(authority)?
            {
                let _ = state.sharing.resume_import(&state, import).await;
            }
            Ok(Json(import_status(&state, import_id).await?))
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RePairRequest {
    expected_lifecycle_generation: i64,
    #[serde(deserialize_with = "invitation_secret")]
    invitation: Secret,
    endpoint_overrides: Option<Vec<Endpoint>>,
}
async fn re_pair(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    let request: RePairRequest = body(raw).await?;
    if request.expected_lifecycle_generation < 1 {
        return Err(invalid());
    }
    let import_id = id(&value)?;
    let import = verified_import(
        &state,
        ImportRequest {
            invitation: request.invitation,
            endpoint_overrides: request.endpoint_overrides,
        },
        import_id,
    )
    .await?;
    let _ = mutation(
        state
            .store
            .re_pair_share_import(import, request.expected_lifecycle_generation)
            .await
            .map_err(authority)?,
    )?;
    if let Some(import) = state
        .store
        .sharing_import(import_id)
        .await
        .map_err(authority)?
    {
        let _ = state.sharing.resume_import(&state, import).await;
    }
    Ok(Json(import_status(&state, import_id).await?))
}
async fn rotate_import(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(value): Path<String>,
    raw: Body,
) -> Result<Json<Value>, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {}
    let _: Request = body(raw).await?;
    let import_id = id(&value)?;
    let import = state
        .store
        .sharing_import(import_id)
        .await
        .map_err(authority)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "sharing_not_found"))?;
    if import.summary.state != "active" {
        return Err(failure(StatusCode::CONFLICT, "sharing_import_inactive"));
    }
    state
        .sharing
        .resume_rotation(&state, import, true)
        .await
        .map_err(peer_error)?;
    Ok(Json(import_status(&state, import_id).await?))
}
