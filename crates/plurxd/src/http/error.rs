//! API error type: every handler returns `Result<_, ApiError>`, and this maps
//! failures to a JSON `{ "error": "..." }` body with the right status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use plurx_core::error::StoreError;
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    NotFound(&'static str),
    BadRequest(String),
    Unauthorized,
    Forbidden,
    Conflict(String),
    UnsupportedMedia(String),
    ServiceUnavailable(String),
    /// A 422 whose body carries more than a sentence — the caller needs the
    /// data to fix the request, not just to read about it. Used for "path is
    /// not under any library root", which lists the roots so a path-mapping
    /// mistake between two applications diagnoses itself.
    Unprocessable(serde_json::Value),
    /// Stable machine-readable errors for APIs whose clients need to choose a
    /// recovery action. Existing endpoints keep their legacy `{error}` body
    /// until their native-client contracts migrate deliberately.
    Typed {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
    /// A typed error whose recovery needs data the client cannot recompute —
    /// the durable film position a lost playback session reached, say. The
    /// extra fields join `code` and `message` in the same object, so a client
    /// that reads only those two is unaffected by their presence.
    TypedDetail {
        status: StatusCode,
        code: &'static str,
        message: String,
        detail: serde_json::Map<String, serde_json::Value>,
    },
    Internal(String),
}

impl ApiError {
    pub fn typed(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Typed {
            status,
            code,
            message: message.into(),
        }
    }

    /// A typed error carrying recovery data alongside its code and message.
    ///
    /// `detail` is data, not a body: `code` and `message` are written last and
    /// win, so a detail field can never rename the error a client dispatches
    /// on. The contract is one flat object, so anything else is dropped and
    /// debug-asserted — a caller that passes an array would otherwise ship a
    /// typed error silently missing the recovery data it exists to carry.
    pub fn typed_detail(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        detail: serde_json::Value,
    ) -> Self {
        debug_assert!(
            detail.is_object(),
            "a typed detail body must be a flat object"
        );
        let detail = match detail {
            serde_json::Value::Object(fields) => fields,
            _ => serde_json::Map::new(),
        };
        Self::TypedDetail {
            status,
            code,
            message: message.into(),
            detail,
        }
    }

    fn parts(&self) -> (StatusCode, String) {
        match self {
            ApiError::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what} not found")),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "authentication required".into()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "admin privileges required".into()),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            ApiError::UnsupportedMedia(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg.clone()),
            ApiError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
            // Handled in `into_response`, which needs the whole body.
            ApiError::Unprocessable(v) => (StatusCode::UNPROCESSABLE_ENTITY, v.to_string()),
            ApiError::Typed {
                status, message, ..
            }
            | ApiError::TypedDetail {
                status, message, ..
            } => (*status, message.clone()),
            ApiError::Internal(msg) => {
                // Detail is logged, not leaked to the client.
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Unprocessable(body) = self {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response();
        }
        if let ApiError::Typed {
            status,
            code,
            message,
        } = self
        {
            return (status, Json(json!({ "code": code, "message": message }))).into_response();
        }
        if let ApiError::TypedDetail {
            status,
            code,
            message,
            mut detail,
        } = self
        {
            // Written last so a detail field cannot shadow the two keys every
            // typed client dispatches on.
            detail.insert("code".to_owned(), json!(code));
            detail.insert("message".to_owned(), json!(message));
            return (status, Json(serde_json::Value::Object(detail))).into_response();
        }
        let (status, message) = self.parts();
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        ApiError::Internal(err.to_string())
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        ApiError::Internal(err.to_string())
    }
}
