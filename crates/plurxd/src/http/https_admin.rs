//! Authenticated administration of the addressed node's HTTPS intent.
use super::{error::ApiError, extract::AdminUser};
use crate::{
    https::{Control, SaveError, Update},
    state::AppState,
};
use axum::{
    extract::{rejection::JsonRejection, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

pub(super) const PATH: &str = "/api/v1/system/https";
pub(super) const CERTIFICATE_PATH: &str = "/api/v1/system/https/certificate";

/// Only these local operations bypass learner/maintenance route exclusions.
/// Authentication and the global removal/serving fences remain in force.
pub(super) fn eligible(method: &Method, path: &str) -> bool {
    (path == PATH && (method == Method::GET || method == Method::PUT))
        || (path == CERTIFICATE_PATH && method == Method::GET)
}

fn control(state: &AppState) -> Result<Arc<Control>, ApiError> {
    state.https.clone().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Node-local HTTPS administration has not been initialized.".into(),
        )
    })
}

pub(super) async fn get(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(control(&state)?.snapshot().await),
    )
        .into_response())
}

pub(super) async fn put(
    _admin: AdminUser,
    State(state): State<AppState>,
    body: Result<Json<Update>, JsonRejection>,
) -> Result<Response, ApiError> {
    // Do not render serde's rejection: unknown keys/type errors may quote
    // arbitrary request text, including a pasted private PEM bundle.
    let Json(update) = body.map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::typed(StatusCode::PAYLOAD_TOO_LARGE, "https_body_too_large", "HTTPS requests must be no larger than 16 KiB.")
        } else {
            ApiError::typed(StatusCode::BAD_REQUEST, "https_invalid_json", "Invalid HTTPS request. Supply node_id, decimal-string revision and desired configuration using the documented fields.")
        }
    })?;
    let snapshot = control(&state)?
        .update(update)
        .await
        .map_err(|error| match error {
            SaveError::Invalid(message) => ApiError::BadRequest(message),
            SaveError::Conflict(code, message) => {
                ApiError::typed(StatusCode::CONFLICT, code, message)
            }
            SaveError::Internal(message) => ApiError::Internal(message.into()),
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(snapshot)).into_response())
}

pub(super) async fn certificate(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let chain = control(&state)?
        .certificate()
        .ok_or(ApiError::NotFound("active HTTPS certificate"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-pem-file"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"plurx-https-certificate.pem\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        chain,
    )
        .into_response())
}
