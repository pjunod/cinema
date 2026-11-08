//! API error type: every handler returns `Result<_, ApiError>`, and this maps
//! failures to a JSON `{ "error": "..." }` body with the right status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use plurx_core::error::StoreError;
use serde_json::json;

#[cfg(target_os = "linux")]
pub(super) static ADAPTIVE_ORIGIN_UNAVAILABLE_JSON: [u8; 68] =
    *br#"{"error":"adaptive origin error response exceeded its paid storage"}"#;

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
    TypedRetry {
        status: StatusCode,
        code: &'static str,
        message: String,
        retry_after_seconds: u64,
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
    /// Shares the original charged diagnostic through HTTP body cancellation.
    FiniteMaterialization(std::sync::Arc<crate::waitpool::FiniteWaitFailure>),
    Internal(String),
    /// Adaptive activation errors use the original core-held backing. Rich
    /// task output has already been destroyed before its funding alias leaves.
    #[cfg(target_os = "linux")]
    AdaptiveOrigin {
        status: StatusCode,
        code: Option<&'static str>,
        retry_after_seconds: Option<u64>,
        body: bytes::Bytes,
    },
    /// Scalar failure when the original paid response cannot be published.
    #[cfg(target_os = "linux")]
    AdaptiveOriginUnavailable,
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

    /// The machine-readable `code` a typed error carries, if it is one.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            ApiError::Typed { code, .. }
            | ApiError::TypedRetry { code, .. }
            | ApiError::TypedDetail { code, .. } => Some(code),
            ApiError::FiniteMaterialization(failure) => Some(failure.code()),
            #[cfg(target_os = "linux")]
            ApiError::AdaptiveOrigin { code, .. } => *code,
            _ => None,
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
            }
            | ApiError::TypedRetry {
                status, message, ..
            } => (*status, message.clone()),
            ApiError::FiniteMaterialization(failure) => {
                (failure.status(), failure.message().to_owned())
            }
            #[cfg(target_os = "linux")]
            ApiError::AdaptiveOrigin { status, .. } => (*status, "adaptive origin error".into()),
            #[cfg(target_os = "linux")]
            ApiError::AdaptiveOriginUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "adaptive origin error response exceeded its paid storage".into(),
            ),
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

/// The typed `code` an [`ApiError`] answered with, carried on the response so
/// a layer that never sees the error itself (the playback failure log in
/// `http::hls::fence`) can still say which refusal a client received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApiErrorCode(pub(crate) &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.code();
        let mut response = self.render();
        if let Some(code) = code {
            response.extensions_mut().insert(ApiErrorCode(code));
        }
        response
    }
}

impl ApiError {
    #[cfg(target_os = "linux")]
    pub(super) fn fund_origin(
        self,
        funding: plurx_core::adaptive_resources::runtime::attempt_root::ReplyPermit,
    ) -> Self {
        // Keep the permit outside the rich error and every temporary JSON DTO.
        // The existing wire status/code/retry/body are retained exactly. The
        // core then owns the original bytes independently of task completion.
        let code = self.code();
        let retry_after_seconds = match &self {
            Self::TypedRetry {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            Self::AdaptiveOrigin {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
            _ => None,
        };
        let (status, encoded) = match &self {
            Self::FiniteMaterialization(failure) => (failure.status(), Ok(failure.body().to_vec())),
            Self::AdaptiveOrigin { status, body, .. } => (*status, Ok(body.to_vec())),
            Self::Unprocessable(body) => {
                (StatusCode::UNPROCESSABLE_ENTITY, serde_json::to_vec(body))
            }
            Self::Typed {
                status,
                code,
                message,
            }
            | Self::TypedRetry {
                status,
                code,
                message,
                ..
            } => (
                *status,
                serde_json::to_vec(&json!({ "code": code, "message": message })),
            ),
            Self::TypedDetail {
                status,
                code,
                message,
                detail,
            } => {
                let mut detail = detail.clone();
                detail.insert("code".to_owned(), json!(code));
                detail.insert("message".to_owned(), json!(message));
                (*status, serde_json::to_vec(&detail))
            }
            _ => {
                let (status, message) = self.parts();
                (status, serde_json::to_vec(&json!({ "error": message })))
            }
        };
        drop(self); // Actual rich output precedes publish and permit release.
        match encoded
            .ok()
            .and_then(|bytes| funding.publish_serialized(bytes).ok())
        {
            Some(body) => Self::AdaptiveOrigin {
                status,
                code,
                retry_after_seconds,
                body,
            },
            None => Self::AdaptiveOriginUnavailable,
        }
    }

    fn render(self) -> Response {
        #[cfg(target_os = "linux")]
        if matches!(self, Self::AdaptiveOriginUnavailable) {
            // Publication already refused and consumed the original permit.
            // Keep this fixed fallback in static storage through transport;
            // constructing another Json body would create unprotected heap.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes::Bytes::from_static(&ADAPTIVE_ORIGIN_UNAVAILABLE_JSON),
            )
                .into_response();
        }
        #[cfg(target_os = "linux")]
        if let Self::AdaptiveOrigin {
            status,
            retry_after_seconds,
            body,
            ..
        } = self
        {
            let mut response = (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response();
            if let Some(seconds) = retry_after_seconds {
                if let Ok(value) = seconds.to_string().parse() {
                    response
                        .headers_mut()
                        .insert(axum::http::header::RETRY_AFTER, value);
                }
            }
            return response;
        }
        if let ApiError::FiniteMaterialization(failure) = self {
            return (
                failure.status(),
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                failure.body(),
            )
                .into_response();
        }
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
        if let ApiError::TypedRetry {
            status,
            code,
            message,
            retry_after_seconds,
        } = self
        {
            let mut response =
                (status, Json(json!({ "code": code, "message": message }))).into_response();
            if let Ok(value) = retry_after_seconds.to_string().parse() {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            return response;
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
