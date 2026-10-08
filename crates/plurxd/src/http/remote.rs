//! Authenticated Cinema companion rendezvous. Owner routing never transfers queues.
mod owner;
pub(crate) mod wire;
use super::{
    error::ApiError,
    extract::{authenticate_token_digest, authenticate_user_token},
    peer_transport::{
        exact_auth_from_headers, signed_response_payload, PeerAuthMode, PeerTransport,
        RESPONSE_SIGNATURE_HEADER,
    },
};
use crate::state::AppState;
use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{stream, StreamExt};
pub(crate) use owner::{Hub, FEATURE_KEY};
use plurx_core::{
    auth,
    remote_control::{Version, MAX_BODY_BYTES},
    store::TokenAudience,
};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use wire::*;
pub(crate) const INTERNAL_PATH: &str = "/internal/remote/v1/dispatch";
pub(crate) fn fail(status: u16, code: &'static str) -> ApiError {
    ApiError::typed(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        code,
        code,
    )
}
pub(crate) fn now_seconds() -> Result<i64, ApiError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| fail(503, "unavailable"))?
            .as_secs(),
    )
    .map_err(|_| fail(503, "unavailable"))
}
pub(crate) fn secret() -> Result<(String, String), ApiError> {
    let raw = auth::generate_token().map_err(|_| fail(503, "unavailable"))?;
    let bytes = hex::decode(raw).map_err(|_| fail(503, "unavailable"))?;
    let raw = URL_SAFE_NO_PAD.encode(bytes);
    let hash = auth::hash_token(&raw);
    Ok((raw, hash))
}
fn proof(headers: &HeaderMap, key: &str) -> Result<Option<String>, ApiError> {
    let mut values = headers.get_all(key).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(fail(401, "unauthorized"));
    }
    let raw = value.to_str().map_err(|_| fail(401, "unauthorized"))?;
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| fail(401, "unauthorized"))?;
    if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(decoded) != raw {
        return Err(fail(401, "unauthorized"));
    }
    Ok(Some(auth::hash_token(raw)))
}
/// Deliberately bypass AuthUser/RawToken's query and X-Api-Key alternatives.
fn bearer(headers: &HeaderMap, uri: &Uri) -> Result<String, ApiError> {
    if uri.query().is_some() || headers.contains_key("x-api-key") {
        return Err(fail(401, "unauthorized"));
    }
    let mut values = headers.get_all("authorization").iter();
    let value = values.next().ok_or_else(|| fail(401, "unauthorized"))?;
    if values.next().is_some() {
        return Err(fail(401, "unauthorized"));
    }
    let token = value
        .to_str()
        .ok()
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 128
                && !v.bytes().any(|b| b.is_ascii_whitespace())
                && !auth::is_api_key(v)
        })
        .ok_or_else(|| fail(401, "unauthorized"))?;
    if let Some(origin) = headers.get("origin") {
        let origin = origin.to_str().map_err(|_| fail(403, "unauthorized"))?;
        let host = headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| fail(403, "unauthorized"))?;
        if origin != format!("http://{host}") && origin != format!("https://{host}") {
            return Err(fail(403, "unauthorized"));
        }
    }
    Ok(token.to_owned())
}
fn parse_post(path: &str, bytes: &[u8]) -> Result<Request, ApiError> {
    macro_rules! parse {
        ($variant:ident) => {
            serde_json::from_slice(bytes)
                .map(Request::$variant)
                .map_err(|_| fail(400, "invalid"))
        };
    }
    match path {
        "receivers" => parse!(CreateReceiver),
        "sessions" => parse!(CreateSession),
        "presence" => parse!(Presence),
        "poll" => parse!(Poll),
        "ack" => parse!(Ack),
        "state" => parse!(ReadState),
        "control" => parse!(Control),
        "commands" => plurx_core::remote_control::Command::decode(bytes)
            .map(Request::Commands)
            .map_err(|_| fail(400, "invalid")),
        "pairing/start" => parse!(PairStart),
        "pairing/claim" => parse!(PairClaim),
        "pairing/approve" => parse!(PairApprove),
        "pairing/result" => parse!(PairResult),
        _ => Err(fail(404, "unavailable")),
    }
}
fn response(status: u16, value: Value) -> Response {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    if bytes.len() > 64 * 1024 {
        return error_response(fail(503, "unavailable"));
    }
    let mut reply = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ],
        bytes,
    )
        .into_response();
    reply.headers_mut().insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    reply
}
fn error_parts(error: ApiError) -> (axum::http::StatusCode, Value) {
    // Remote endpoints use a versioned error dialect; preserve meaningful HTTP
    // status but keep diagnostics generic, with no token/secret in text.
    let explicit = error.code();
    let reply = error.into_response();
    let status = reply.status();
    let code = explicit.unwrap_or(match status.as_u16() {
        401 | 403 => "unauthorized",
        400 | 413 | 422 => "invalid",
        409 | 429 => "busy",
        _ => "unavailable",
    });
    (
        status,
        json!({"version":"cinema.remote.v1","code":code,"message":code}),
    )
}
fn error_response(error: ApiError) -> Response {
    let (status, value) = error_parts(error);
    (status, [("cache-control", "no-store")], axum::Json(value)).into_response()
}
async fn enabled(state: &AppState) -> Result<(), ApiError> {
    if state.membership.local_maintenance_active() {
        return Err(fail(503, "unavailable"));
    }
    if state
        .store
        .get_setting(owner::FEATURE_KEY)
        .await?
        .as_deref()
        != Some("1")
    {
        state.remote.disable();
        return Err(fail(503, "unavailable"));
    }
    Ok(())
}
async fn route(state: &AppState, dispatch: Dispatch) -> Result<(u16, Value), ApiError> {
    if let Some(target) = dispatch.request.target() {
        if target.owner_node_id != state.node_id {
            let peers = state
                .membership
                .operations_peers()
                .await
                .map_err(|_| fail(503, "unavailable"))?;
            let peer = peers
                .into_iter()
                .find(|p| p.node_id == target.owner_node_id && p.reachable)
                .ok_or_else(|| fail(503, "unavailable"))?;
            let base = peer.http_base.ok_or_else(|| fail(503, "unavailable"))?;
            let body = serde_json::to_vec(&dispatch).map_err(|_| fail(400, "invalid"))?;
            let response = PeerTransport::new(state.membership.clone())
                .request(
                    &peer.node_id,
                    &base,
                    reqwest::Method::POST,
                    INTERNAL_PATH,
                    body,
                    tokio::time::Instant::now() + Duration::from_secs(25),
                    64 * 1024,
                    PeerAuthMode::ExactRequestAndMemberResponse,
                )
                .await
                .map_err(|_| fail(503, "unavailable"))?;
            let value: Value =
                serde_json::from_slice(&response.body).map_err(|_| fail(503, "unavailable"))?;
            if value["version"] != "cinema.remote.v1" {
                return Err(fail(503, "unavailable"));
            }
            return Ok((response.status.as_u16(), value));
        }
    }
    state.remote.perform(state, dispatch).await
}
async fn listings(state: &AppState, d: &Dispatch) -> Result<Value, ApiError> {
    let receivers = state.store.remote_receivers(d.user_id).await?;
    let grants = state.store.remote_grants(d.user_id).await?;
    let (_, local) = state
        .remote
        .perform(
            state,
            Dispatch {
                request: Request::ListSessions {
                    version: Version::V1,
                },
                ..d.clone()
            },
        )
        .await?;
    let mut summaries = local["sessions"].as_array().cloned().unwrap_or_default();
    let mut unavailable = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let peers = tokio::time::timeout_at(deadline, state.membership.operations_peers())
        .await
        .map_err(|_| fail(503, "unavailable"))?
        .map_err(|_| fail(503, "unavailable"))?;
    let transport = PeerTransport::new(state.membership.clone());
    let body = serde_json::to_vec(&Dispatch {
        request: Request::ListSessions {
            version: Version::V1,
        },
        ..d.clone()
    })
    .map_err(|_| fail(400, "invalid"))?;
    let replies = stream::iter(peers.into_iter().map(|peer| {
        let transport = transport.clone();
        let body = body.clone();
        async move {
            let result = if let Some(base) = peer.http_base {
                transport
                    .request(
                        &peer.node_id,
                        &base,
                        reqwest::Method::POST,
                        INTERNAL_PATH,
                        body,
                        deadline,
                        64 * 1024,
                        PeerAuthMode::ExactRequestAndMemberResponse,
                    )
                    .await
                    .ok()
            } else {
                None
            };
            (peer.node_id, result)
        }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    for (node, reply) in replies {
        if let Some(reply) = reply.filter(|r| r.status.is_success()) {
            if let Ok(value) = serde_json::from_slice::<Value>(&reply.body) {
                if let Some(list) = value["sessions"].as_array() {
                    summaries.extend(list.iter().cloned());
                    continue;
                }
            }
        }
        unavailable.push(node);
    }
    let receivers=receivers.into_iter().map(|r|{let live=unique_summary(&summaries,&r.id);json!({"receiver_id":r.id,"name":r.name,"platform":r.platform,"target":live.map(|s|s["target"].clone()),"available":live.is_some(),"busy":live.is_some_and(|s|s["busy"]==true),"paired":grants.iter().any(|g|g.receiver_id==r.id)})}).collect::<Vec<_>>();
    Ok(json!({"version":"cinema.remote.v1","receivers":receivers,"unavailable_nodes":unavailable}))
}
// Two owners may briefly advertise one installation after reconnect. Never
// choose a target based on local-first or nondeterministic peer response order.
fn unique_summary<'a>(summaries: &'a [Value], receiver: &str) -> Option<&'a Value> {
    let mut matching = summaries.iter().filter(|s| s["receiver_id"] == receiver);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}
pub(crate) async fn public(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match public_inner(&state, &uri, method, &headers, &body).await {
        Ok((status, value)) => response(status, value),
        Err(error) => error_response(error),
    }
}
async fn public_inner(
    state: &AppState,
    uri: &Uri,
    method: Method,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(u16, Value), ApiError> {
    let cap = if uri.path() == "/api/remote/v1/presence" {
        64 * 1024
    } else {
        MAX_BODY_BYTES
    };
    if body.len() > cap {
        return Err(fail(413, "invalid"));
    }
    let token = bearer(headers, uri)?;
    let user = authenticate_user_token(state, &token).await?;
    enabled(state).await?;
    let path = uri
        .path()
        .strip_prefix("/api/remote/v1/")
        .ok_or_else(|| fail(404, "unavailable"))?;
    let proof = Proof {
        receiver_hash: proof(headers, "x-cinema-receiver-secret")?,
        grant_hash: proof(headers, "x-cinema-grant-secret")?,
        pairing_hash: proof(headers, "x-cinema-pairing-secret")?,
    };
    let d = Dispatch {
        version: Version::V1,
        user_id: user.id,
        token_digest: auth::hash_token(&token),
        proof,
        request: Request::ListSessions {
            version: Version::V1,
        },
    };
    if method == Method::GET {
        if !body.is_empty() {
            return Err(fail(400, "invalid"));
        }
        return match path {
            "receivers" => Ok((200, listings(state, &d).await?)),
            "grants" => Ok((
                200,
                json!({"version":"cinema.remote.v1","grants":state.store.remote_grants(user.id).await?}),
            )),
            _ => Err(fail(404, "unavailable")),
        };
    }
    if method == Method::DELETE {
        if !body.is_empty() {
            return Err(fail(400, "invalid"));
        }
        let (kind, id) = path
            .split_once('/')
            .ok_or_else(|| fail(404, "unavailable"))?;
        let id = Uuid::parse_str(id)
            .map_err(|_| fail(400, "invalid"))?
            .to_string();
        match kind {
            "receivers" => {
                state
                    .store
                    .revoke_remote_receiver(&id, user.id, now_seconds()?)
                    .await?;
                state.remote.revoke_installation(user.id, &id);
            }
            "grants" => {
                state
                    .store
                    .revoke_remote_grant(&id, user.id, now_seconds()?)
                    .await?;
                state.remote.revoke_grant(user.id, &id)?;
            }
            _ => return Err(fail(404, "unavailable")),
        };
        // Wake local polls; remote long polls recheck authority every second.
        state.remote.changed.notify_waiters();
        return Ok((200, json!({"version":"cinema.remote.v1","revoked":true})));
    }
    if method != Method::POST
        || body.len()
            > if path == "presence" {
                64 * 1024
            } else {
                MAX_BODY_BYTES
            }
    {
        return Err(fail(400, "invalid"));
    }
    let request = parse_post(path, body)?;
    let proofs = &d.proof;
    let receiver = request.receiver_side();
    let grant = request.grant_id().is_some();
    let pairing = matches!(request, Request::PairResult(_));
    if proofs.receiver_hash.is_some() != receiver
        || proofs.grant_hash.is_some() != grant
        || proofs.pairing_hash.is_some() != pairing
    {
        return Err(fail(401, "unauthorized"));
    }

    if request.wait_ms() > 20_000 {
        return Err(fail(400, "invalid"));
    }
    route(state, Dispatch { request, ..d }).await
}
pub(crate) async fn internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = match exact_auth_from_headers(&headers) {
        Some(auth) => auth,
        None => return error_response(fail(401, "unauthorized")),
    };
    if !state
        .membership
        .authorize_internal_peer_request(&auth, "POST", INTERNAL_PATH, &body)
        .await
        .unwrap_or(false)
    {
        return error_response(fail(401, "unauthorized"));
    }
    let result = async {
        enabled(&state).await?;
        let d: Dispatch = serde_json::from_slice(&body).map_err(|_| fail(400, "invalid"))?;
        if d.token_digest.len() != 64
            || !d.token_digest.bytes().all(|b| b.is_ascii_hexdigit())
            || d.request
                .target()
                .is_some_and(|t| t.owner_node_id != state.node_id)
        {
            return Err(fail(400, "invalid"));
        }
        let user = authenticate_token_digest(&state, d.token_digest.clone(), TokenAudience::Native)
            .await?;
        if user.id != d.user_id {
            return Err(fail(403, "unauthorized"));
        }
        state.remote.perform(&state, d).await
    }
    .await;
    let (status, value) = match result {
        Ok(reply) => reply,
        Err(error) => {
            let (status, value) = error_parts(error);
            (status.as_u16(), value)
        }
    };
    let bytes = match serde_json::to_vec(&value) {
        Ok(bytes) if bytes.len() <= 64 * 1024 => bytes,
        _ => return error_response(fail(503, "unavailable")),
    };
    let payload = signed_response_payload(status, &bytes);
    let signature = match state.membership.sign_internal_peer_response(
        &auth.node_id,
        &auth.nonce,
        INTERNAL_PATH,
        &payload,
    ) {
        Ok(signature) => signature,
        Err(_) => return error_response(fail(503, "unavailable")),
    };
    Response::builder()
        .status(status)
        .header(RESPONSE_SIGNATURE_HEADER, signature)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| error_response(fail(503, "unavailable")))
}
pub(crate) fn eligible(method: &Method, path: &str) -> bool {
    if method == Method::POST && path == INTERNAL_PATH {
        return true;
    }
    let Some(path) = path.strip_prefix("/api/remote/v1/") else {
        return false;
    };
    match *method {
        Method::POST => matches!(
            path,
            "receivers"
                | "sessions"
                | "presence"
                | "poll"
                | "ack"
                | "state"
                | "control"
                | "commands"
                | "pairing/start"
                | "pairing/claim"
                | "pairing/approve"
                | "pairing/result"
        ),
        Method::GET => matches!(path, "receivers" | "grants"),
        Method::DELETE => path.split_once('/').is_some_and(|(kind, id)| {
            matches!(kind, "receivers" | "grants") && Uuid::parse_str(id).is_ok()
        }),
        _ => false,
    }
}

pub(crate) fn router() -> axum::Router<AppState> {
    axum::Router::new()
        // The outer router also serves Plex's literal colon routes. Preserve
        // its existing compatibility setting when merging this router.
        .without_v07_checks()
        .route(
            "/api/remote/v1/{*path}",
            axum::routing::get(public)
                .post(public)
                .delete(public)
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            INTERNAL_PATH,
            axum::routing::post(internal).layer(axum::extract::DefaultBodyLimit::max(68 * 1024)),
        )
        .layer(axum::middleware::from_fn(limit_response))
}
async fn limit_response(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return error_response(fail(413, "invalid"));
    }
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
#[path = "remote/tests.rs"]
mod tests;
