//! Explicit consent for an optional household-only application login.
use super::{
    error::ApiError,
    extract::{AdminUser, AuthUser, RawToken},
};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use plurx_core::{
    auth,
    store::{
        delegation::{DelegationClient, DelegationCode, DelegationRedemption},
        JellyfinEntityKind,
    },
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
const SCOPE: &str = "household-requests";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    name: String,
    redirect: String,
}
fn callback(value: &str) -> Result<reqwest::Url, ApiError> {
    let u = reqwest::Url::parse(value)
        .map_err(|_| ApiError::BadRequest("exact HTTPS callback required".into()))?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
    {
        return Err(ApiError::BadRequest("exact HTTPS callback required".into()));
    }
    Ok(u)
}
pub async fn register(
    AdminUser(_): AdminUser,
    State(s): State<AppState>,
    Json(r): Json<Register>,
) -> Result<Json<serde_json::Value>, ApiError> {
    callback(&r.redirect)?;
    if r.name.trim().is_empty() || r.name.len() > 100 {
        return Err(ApiError::BadRequest(
            "client name required (100 bytes maximum)".into(),
        ));
    }
    let secret = auth::generate_token()
        .map_err(|_| ApiError::Internal("random credential unavailable".into()))?;
    let c = DelegationClient {
        id: uuid::Uuid::new_v4().to_string(),
        name: r.name,
        redirect: r.redirect,
        generation: 1,
    };
    s.store
        .delegation_register(c.clone(), auth::hash_token(&secret))
        .await?;
    Ok(Json(
        serde_json::json!({"client":c,"clientSecret":secret,"issuer":s.store.instance_id().await?}),
    ))
}
pub async fn clients(
    AdminUser(_): AdminUser,
    State(s): State<AppState>,
) -> Result<Json<Vec<DelegationClient>>, ApiError> {
    Ok(Json(s.store.delegation_clients().await?))
}
pub async fn disconnect(
    AdminUser(_): AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.store.delegation_disconnect(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InfoQuery {
    client_id: String,
}
pub async fn info(
    State(s): State<AppState>,
    Query(q): Query<InfoQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s
        .store
        .delegation_client(&q.client_id)
        .await?
        .ok_or(ApiError::NotFound("application connection"))?;
    Ok(Json(
        serde_json::json!({"issuer":s.store.instance_id().await?,"client":c,"scope":SCOPE}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Consent {
    client_id: String,
    challenge: String,
    state: String,
}
pub async fn consent(
    AuthUser(u): AuthUser,
    RawToken(token): RawToken,
    State(s): State<AppState>,
    Json(r): Json<Consent>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if r.state.len() < 32
        || r.state.len() > 256
        || r.challenge.len() != 43
        || URL_SAFE_NO_PAD.decode(&r.challenge).is_err()
    {
        return Err(ApiError::BadRequest("invalid consent transaction".into()));
    }
    let c = s
        .store
        .delegation_client(&r.client_id)
        .await?
        .ok_or(ApiError::NotFound("application connection"))?;
    let subjects = s
        .store
        .jellyfin_entity_ids(JellyfinEntityKind::User, &[u.id])
        .await?;
    let subject = subjects
        .first()
        .ok_or(ApiError::Unauthorized)?
        .wire_id
        .clone();
    let code = auth::generate_handoff_code()
        .map_err(|_| ApiError::Internal("random credential unavailable".into()))?;
    if !s
        .store
        .delegation_mint(DelegationCode {
            code_hash: auth::hash_token(&code),
            client_id: r.client_id,
            user_id: u.id,
            subject,
            source_hash: auth::hash_token(&token),
            challenge: r.challenge,
        })
        .await?
    {
        return Err(ApiError::Unauthorized);
    }
    let issuer = s.store.instance_id().await?;
    let mut redirect = callback(&c.redirect)?;
    redirect
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &r.state)
        .append_pair("iss", &issuer);
    Ok(Json(serde_json::json!({"redirect":redirect.as_str()})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Exchange {
    client_id: String,
    client_secret: String,
    code: String,
    verifier: String,
    redirect: String,
}
async fn client_auth(s: &AppState, id: &str, secret: &str) -> Result<(), ApiError> {
    if id.len() > 80
        || secret.len() > 256
        || !s
            .store
            .delegation_client_authorized(id, &auth::hash_token(secret))
            .await?
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}
pub async fn exchange(
    State(s): State<AppState>,
    Json(r): Json<Exchange>,
) -> Result<Json<serde_json::Value>, ApiError> {
    client_auth(&s, &r.client_id, &r.client_secret).await?;
    if !auth::is_handoff_code(&r.code)
        || !(43..=128).contains(&r.verifier.len())
        || !r
            .verifier
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._~".contains(&c))
    {
        return Err(ApiError::BadRequest("invalid code or verifier".into()));
    }
    let grant = auth::generate_token()
        .map_err(|_| ApiError::Internal("random credential unavailable".into()))?;
    let identity = s
        .store
        .delegation_redeem(DelegationRedemption {
            code_hash: auth::hash_token(&r.code),
            client_id: r.client_id.clone(),
            secret_hash: auth::hash_token(&r.client_secret),
            redirect: r.redirect,
            challenge: URL_SAFE_NO_PAD.encode(Sha256::digest(r.verifier.as_bytes())),
            grant_hash: auth::hash_token(&grant),
            claim: uuid::Uuid::new_v4().to_string(),
        })
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(
        identity_response(&s, r.client_id, Some(grant), identity).await?,
    ))
}
async fn identity_response(
    s: &AppState,
    client: String,
    grant: Option<String>,
    identity: plurx_core::store::delegation::DelegationIdentity,
) -> Result<serde_json::Value, ApiError> {
    Ok(
        serde_json::json!({"active":true,"iss":s.store.instance_id().await?,"sub":identity.subject,"aud":client,"scope":SCOPE,"displayName":identity.display_name,"expiresAt":identity.expires_at,"grant":grant}),
    )
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantRequest {
    client_id: String,
    client_secret: String,
    grant: String,
}
pub async fn introspect(
    State(s): State<AppState>,
    Json(r): Json<GrantRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    client_auth(&s, &r.client_id, &r.client_secret).await?;
    if r.grant.len() > 256 {
        return Err(ApiError::Unauthorized);
    }
    match s
        .store
        .delegation_introspect(
            &r.client_id,
            &auth::hash_token(&r.client_secret),
            &auth::hash_token(&r.grant),
        )
        .await?
    {
        Some(identity) => Ok(Json(
            identity_response(&s, r.client_id, None, identity).await?,
        )),
        None => Ok(Json(serde_json::json!({"active":false}))),
    }
}
pub async fn revoke(
    State(s): State<AppState>,
    Json(r): Json<GrantRequest>,
) -> Result<StatusCode, ApiError> {
    client_auth(&s, &r.client_id, &r.client_secret).await?;
    s.store
        .delegation_revoke(
            &r.client_id,
            &auth::hash_token(&r.client_secret),
            &auth::hash_token(&r.grant),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> axum::Router<AppState> {
    use axum::{
        extract::DefaultBodyLimit,
        http::HeaderValue,
        middleware,
        routing::{delete, get, post},
    };
    axum::Router::new()
        .route("/clients", get(clients).post(register))
        .route("/clients/{id}", delete(disconnect))
        .route("/info", get(info))
        .route("/consent", post(consent))
        .route("/exchange", post(exchange))
        .route("/introspect", post(introspect))
        .route("/revoke", post(revoke))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(
            async |request: axum::extract::Request, next: middleware::Next| {
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert("cache-control", HeaderValue::from_static("no-store"));
                response
                    .headers_mut()
                    .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
                response
            },
        ))
}
