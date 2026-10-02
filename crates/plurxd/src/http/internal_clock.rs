//! Authenticated four-timestamp response; no enforcement or Store consumer.
use crate::state::AppState;
use axum::{
    body::Body,
    extract::State,
    http::{header, Request, Response, StatusCode},
};
use serde::{Deserialize, Serialize};

pub(crate) const PATH: &str = "/_internal/v1/clock";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClockResponse {
    pub node_id: String,
    pub received_unix_ms: i64,
    pub sent_unix_ms: i64,
}

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    snapshot_for_membership(&state.membership, &state.node_id, request).await
}

/// One existing exact-request authorizer, shared by pending observation and
/// normal service. This context deliberately owns no application services.
#[derive(Clone)]
pub(crate) struct ObservationContext {
    pub(crate) membership: plurx_core::cluster::membership::MembershipManager,
    pub(crate) node_id: String,
}

pub(crate) async fn observation_snapshot(
    State(state): State<ObservationContext>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    snapshot_for_membership(&state.membership, &state.node_id, request).await
}

async fn snapshot_for_membership(
    membership: &plurx_core::cluster::membership::MembershipManager,
    node_id: &str,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    let received_unix_ms = crate::media_sessions::unix_ms();
    let auth = super::peer_transport::exact_auth_from_headers(request.headers())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // The exact proof is for an empty GET. Never ignore an unsigned raw body.
    axum::body::to_bytes(request.into_body(), 0)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !membership
        .authorize_internal_peer_request(&auth, "GET", PATH, &[])
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let sent_unix_ms = crate::media_sessions::unix_ms();
    let body = serde_json::to_vec(&ClockResponse {
        node_id: node_id.to_owned(),
        received_unix_ms,
        sent_unix_ms,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let payload = super::peer_transport::signed_response_payload(StatusCode::OK.as_u16(), &body);
    let signature = membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, PATH, &payload)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(super::peer_transport::RESPONSE_SIGNATURE_HEADER, signature)
        .body(Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
