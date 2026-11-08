//! `POST /api/v1/scan` — "scan exactly this path", for other applications.
//!
//! The fast path in the Curator pipeline: when Curator finishes importing into
//! a library folder it says so, instead of plurx discovering it on the next
//! scheduled sweep. Key-scoped, never user-token: see
//! [`crate::http::extract::ScopedKey`] for why the wall runs both ways.

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use plurx_core::domain::scopes;
use plurx_core::scan::containment::{self, PathClass};
use plurx_core::scan::TargetError;
use plurx_core::store::scan_request_v2::{ExpectedFile, ScanRequestV2Seed};
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::ScopedKey;
use crate::state::{AppState, BookHints, IdHints, ScanAdmission, ScanRequest};

#[derive(Debug, Deserialize, Default)]
pub struct Ids {
    #[serde(default)]
    pub tmdb: Option<i64>,
    #[serde(default)]
    pub imdb: Option<String>,
}

/// Explicit metadata for a Curator-managed book edition. These fields are
/// facts Curator already has at import time; carrying them avoids a second
/// provider match and, importantly, gives Cinema proof for relating ebook and
/// audiobook editions without guessing from title + author.
#[derive(Debug, Deserialize)]
pub struct BookMetadataBody {
    pub title: Option<String>,
    pub author: Option<String>,
    pub medium: String,
    pub work_id: String,
    pub edition_id: String,
    pub cover_url: Option<String>,
}

/// One file the caller placed under `path` (contract v2 §4.2).
#[derive(Debug, Deserialize)]
pub struct ExpectedFileBody {
    pub path: String,
    pub bytes: i64,
    pub mtime_ns: i64,
}

#[derive(Debug, Deserialize)]
pub struct ScanRequestBody {
    /// Absolute path — a directory or a single file. Optional only so that a
    /// `probe_paths` connection test can omit it; a scan without it is a 400
    /// `path_required` from the handler, not the extractor's 422.
    #[serde(default)]
    pub path: Option<String>,
    /// Curator's idempotency key, `<transfer>/<generation>/<dir>/<attempt>`.
    /// A repeated key returns the request it created; a new attempt suffix
    /// is a new request.
    #[serde(default)]
    pub request_key: Option<String>,
    /// Hold the response up to this long (capped at 15 s) for the result.
    #[serde(default)]
    pub wait_ms: Option<u64>,
    /// The media files this import placed under `path`. Each is probed
    /// whatever its size and mtime say, and reported per file.
    #[serde(default)]
    pub expected_files: Option<Vec<ExpectedFileBody>>,
    #[serde(default)]
    pub ids: Option<Ids>,
    /// `movie` | `episode` | `season` | `book`. Advisory: the library's own kind
    /// decides how a file is parsed, so this is only used to pick which item an
    /// id applies to. Exact Curator book identity travels in `book`, not here.
    #[serde(default)]
    pub hint: Option<String>,
    /// For episodes, the id of the SHOW — an episode's own tmdb id is not
    /// what identifies the series it belongs to.
    #[serde(default)]
    pub series: Option<Ids>,
    /// Present on paired Curator book imports. Omitted by older Curator builds
    /// and by every non-book caller.
    #[serde(default)]
    pub book: Option<BookMetadataBody>,
    /// Echoed back and logged, so one grep reconstructs a transfer across
    /// every application it passed through.
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Serialize)]
pub struct ScanAccepted {
    pub status: &'static str,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

/// The longest a `POST /scan` holds its response for a result.
pub const MAX_WAIT_MS: u64 = 15_000;
/// How long a caller should wait before retrying a `path_unreadable`.
pub const PATH_UNREADABLE_RETRY_S: u64 = 60;

/// POST /api/v1/scan (scope `scan:trigger`)
pub async fn scan(
    State(state): State<AppState>,
    parts: Parts,
    Json(body): Json<ScanRequestBody>,
) -> Result<Response, ApiError> {
    let mut parts = parts;
    let key = ScopedKey::require(&mut parts, &state, scopes::SCAN_TRIGGER).await?;
    // Counted here, before the path is resolved: a request rejected for a
    // path-mapping mistake still proves the other application reached plurx
    // with a working key, and that is the first thing anyone debugging this
    // needs to rule in or out.
    state.jobs.metrics().count_notification();

    let Some(raw_path) = body.path.clone() else {
        return Err(ApiError::Typed {
            status: StatusCode::BAD_REQUEST,
            code: "path_required",
            message: "`path` is required unless the request is a `probe_paths` test".into(),
        });
    };
    let path = PathBuf::from(&raw_path);
    if !path.is_absolute() {
        return Err(ApiError::BadRequest(format!(
            "`{raw_path}` is not an absolute path — plurx and the caller may not share a \
             working directory, so relative paths cannot be resolved"
        )));
    }
    let request_key = body.request_key.clone().unwrap_or_default();
    let expected_files = body
        .expected_files
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|file| ExpectedFile {
            path: file.path.clone(),
            bytes: file.bytes,
            mtime_ns: file.mtime_ns,
        })
        .collect::<Vec<_>>();
    if expected_files
        .iter()
        .any(|file| !std::path::Path::new(&file.path).is_absolute())
    {
        return Err(ApiError::BadRequest(
            "every `expected_files[].path` must be absolute".to_owned(),
        ));
    }
    let seed = ScanRequestV2Seed {
        key_id: key.0.id,
        request_key,
        expected_files,
    };
    seed.validate()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let wait = Duration::from_millis(body.wait_ms.unwrap_or(0).min(MAX_WAIT_MS));

    // Idempotency first: a lost response must not become a second request.
    if !seed.request_key.is_empty() {
        if let Some(existing) = state
            .store
            .scan_request_by_key(seed.key_id, &seed.request_key)
            .await?
        {
            return answer(&state, &existing, wait, body.correlation_id.clone()).await;
        }
    }
    if body
        .series
        .as_ref()
        .and_then(|series| series.tmdb)
        .is_some_and(|tmdb_id| tmdb_id <= 0)
    {
        return Err(ApiError::BadRequest(
            "`series.tmdb` must be a positive integer".to_owned(),
        ));
    }

    // plurx resolves the library, so the caller never needs to know plurx's
    // library ids — one less thing to keep in sync between two applications.
    // Containment is lexical first, so "not ours" (permanent) is never
    // confused with "ours, but the share is not mounted right now".
    let libraries = state.store.list_libraries().await?;
    let classified = {
        let libraries = libraries.clone();
        let path = path.clone();
        tokio::task::spawn_blocking(
            move || match containment::classify_path(&libraries, &path) {
                PathClass::Outside { roots } => Err(Err(roots)),
                PathClass::Unreadable { root, error } => Err(Ok((root, error))),
                PathClass::Inside { library, .. } => Ok(library.id),
            },
        )
        .await
        .map_err(|error| ApiError::Internal(format!("path classification: {error}")))?
    };
    let library = match classified {
        Ok(id) => libraries
            .iter()
            .find(|library| library.id == id)
            .ok_or_else(|| ApiError::Internal("classified library vanished".into()))?,
        Err(Err(roots)) => return Err(outside_roots(&raw_path, roots)),
        Err(Ok((root, error))) => return Ok(path_unreadable(&raw_path, &root, &error)),
    };

    let book = body.book.as_ref().map(validate_book_hints).transpose()?;
    if book.is_some() && library.kind != plurx_core::domain::LibraryKind::Books {
        return Err(ApiError::BadRequest(
            "`book` metadata is accepted only for a Books library path".to_owned(),
        ));
    }

    let request_id = format!("sr-{}", uuid::Uuid::new_v4().simple());
    // Persist hints with the request before acknowledging it. An eligible
    // cluster worker applies them when it scans, including after a restart.
    let episodeish = matches!(body.hint.as_deref(), Some("episode") | Some("season"))
        || body.series.as_ref().is_some_and(|s| s.tmdb.is_some());
    // A series import deliberately carries the SHOW id in `series`, with no
    // item-level `ids` object. Keying this Option off `body.ids` alone drops
    // exactly the hint TV imports send, leaving the targeted scan to guess
    // from the folder name instead of using the manager's authoritative id.
    let ids = (body.ids.is_some() || body.series.is_some()).then(|| IdHints {
        tmdb: body.ids.as_ref().and_then(|i| i.tmdb),
        imdb: body.ids.as_ref().and_then(|i| i.imdb.clone()),
        series_tmdb: body.series.as_ref().and_then(|s| s.tmdb),
        episodeish,
    });
    let req = ScanRequest {
        id: request_id.clone(),
        library_id: library.id,
        path: path.clone(),
        ids,
        book,
        correlation_id: body.correlation_id.clone(),
        source: body.source.clone(),
    };

    // The sidecar is written in the admission transaction itself, so even a
    // worker that claims and finishes this request before the response is
    // built probes the expected files (contract v2 §5.1).
    let request_id = match state.jobs.request_scan_v2(req, seed).await {
        Ok(ScanAdmission::Admitted(id) | ScanAdmission::Replay(id)) => id,
        Err(TargetError::OutsideRoots { path, roots }) => return Err(outside_roots(&path, roots)),
        Err(TargetError::Unreadable { path, root, error }) => {
            return Ok(path_unreadable(&path, std::path::Path::new(&root), &error))
        }
        // Refused, not failed: the queue is full or the request is being
        // cancelled. 503 tells a caller like Monarr to retry later, where a
        // 500 would tell it plurx is broken.
        Err(error @ TargetError::Refused(_)) => {
            return Err(ApiError::ServiceUnavailable(error.to_string()))
        }
        Err(TargetError::Store(e)) => return Err(ApiError::from(e)),
    };
    answer(&state, &request_id, wait, body.correlation_id).await
}

/// The answer for one request: `200 scanned` with its result and per-file
/// report once it is done, `202 queued` while it is not (after waiting up to
/// `wait` for it), and the failure a finished request recorded.
async fn answer(
    state: &AppState,
    request_id: &str,
    wait: Duration,
    correlation_id: Option<String>,
) -> Result<Response, ApiError> {
    let record = if wait.is_zero() {
        state.jobs.scan_request(request_id).await
    } else {
        state.jobs.await_scan_request(request_id, wait).await
    };
    let record = record.ok_or(ApiError::NotFound("scan request"))?;
    let correlation_id = record.correlation_id.clone().or(correlation_id);
    match record.status.as_str() {
        "done" => {
            let files = state
                .store
                .scan_request_v2(request_id)
                .await?
                .map(|row| row.files)
                .unwrap_or_default();
            Ok((
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "scanned",
                    "library_id": record.library_id,
                    "request_id": record.request_id,
                    "report": record.report,
                    "items": record.items,
                    "files": files,
                    "correlation_id": correlation_id,
                })),
            )
                .into_response())
        }
        // A finished failure answers with the request it is — the same
        // `request_id`, `status` and `error` the GET record carries — so a
        // replayed `request_key` returns its result (contract v2 §5.1) and
        // the caller can tell a transient root from any other failure.
        "failed" => {
            let error = record.error.unwrap_or_default();
            if error.starts_with(PATH_UNREADABLE) {
                // The share dropped between admission and the scan.
                return Ok(path_unreadable_for(
                    &record.path,
                    std::path::Path::new(""),
                    &error,
                    Some(&record.request_id),
                ));
            }
            let mut detail = serde_json::Map::new();
            detail.insert("status".into(), serde_json::json!("failed"));
            detail.insert("request_id".into(), serde_json::json!(record.request_id));
            detail.insert("error".into(), serde_json::json!(error));
            if let Some(correlation_id) = correlation_id {
                detail.insert("correlation_id".into(), serde_json::json!(correlation_id));
            }
            Err(ApiError::TypedDetail {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "scan_failed",
                message: "the scan request failed; see `error`".into(),
                detail,
            })
        }
        // Busy: queued rather than dropped. 202 with an id to poll, because
        // silently losing the request would leave a season half-indexed.
        _ => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(ScanAccepted {
                status: "queued",
                request_id: record.request_id,
                correlation_id,
            })?),
        )
            .into_response()),
    }
}

/// The error prefix a request failed by an unreadable root records.
const PATH_UNREADABLE: &str = "path_unreadable";

/// `422 path_outside_roots` — permanent. Self-explaining on purpose: a
/// path-mapping mistake between two containers is the likeliest cause by a
/// wide margin, and naming the roots turns a support conversation into a
/// glance. `error` and `roots` keep their old meaning for older callers.
fn outside_roots(path: &str, roots: Vec<String>) -> ApiError {
    ApiError::Unprocessable(serde_json::json!({
        "code": "path_outside_roots",
        "error": "path is not under any library root",
        "path": path,
        "roots": roots,
    }))
}

/// `503 path_unreadable` — transient: the path is under a library root that
/// cannot be read right now (an unmounted share, `ENOTCONN`, `EIO`), so the
/// same request succeeds once it is back.
fn path_unreadable(path: &str, root: &std::path::Path, error: &str) -> Response {
    path_unreadable_for(path, root, error, None)
}

/// [`path_unreadable`] for a request that was admitted and then failed: the
/// body also names it, as `status: failed` with its `request_id`.
fn path_unreadable_for(
    path: &str,
    root: &std::path::Path,
    error: &str,
    request_id: Option<&str>,
) -> Response {
    let mut body = serde_json::json!({
        "code": PATH_UNREADABLE,
        "error": format!("library root could not be read: {error}"),
        "path": path,
        "retry_after_s": PATH_UNREADABLE_RETRY_S,
    });
    if !root.as_os_str().is_empty() {
        body["root"] = serde_json::json!(root.display().to_string());
    }
    if let Some(request_id) = request_id {
        body["request_id"] = serde_json::json!(request_id);
        body["status"] = serde_json::json!("failed");
    }
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from(PATH_UNREADABLE_RETRY_S),
    );
    response
}

fn validate_book_hints(body: &BookMetadataBody) -> Result<BookHints, ApiError> {
    let medium = match body.medium.as_str() {
        "ebook" => plurx_core::domain::ItemKind::Book,
        "audiobook" => plurx_core::domain::ItemKind::Audiobook,
        _ => {
            return Err(ApiError::BadRequest(
                "`book.medium` must be `ebook` or `audiobook`".to_owned(),
            ))
        }
    };
    let text = |field: &str, value: &str, max: usize| -> Result<String, ApiError> {
        let value = value.trim();
        if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
            return Err(ApiError::BadRequest(format!(
                "`book.{field}` must be 1..={max} non-control UTF-8 bytes"
            )));
        }
        Ok(value.to_owned())
    };
    let id = |field: &str, value: &str| -> Result<String, ApiError> {
        let value = text(field, value, 256)?;
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b":._-/".contains(&byte))
        {
            return Err(ApiError::BadRequest(format!(
                "`book.{field}` contains an unsupported identifier character"
            )));
        }
        Ok(value)
    };
    let cover_url = body
        .cover_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    if cover_url.is_some_and(|url| {
        url.len() > 2_048 || plurx_core::metadata::book::allowed_curator_cover_url(url).is_none()
    }) {
        return Err(ApiError::BadRequest(
            "`book.cover_url` must be an Open Library HTTPS cover URL".to_owned(),
        ));
    }
    Ok(BookHints {
        title: body
            .title
            .as_deref()
            .map(|value| text("title", value, 512))
            .transpose()?,
        author: body
            .author
            .as_deref()
            .map(|value| text("author", value, 512))
            .transpose()?,
        medium,
        work_id: id("work_id", &body.work_id)?,
        edition_id: id("edition_id", &body.edition_id)?,
        cover_url: cover_url.map(str::to_owned),
    })
}

/// GET /api/v1/scan/requests/{id} (scope `status:read`)
pub async fn request_status(
    State(state): State<AppState>,
    parts: Parts,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut parts = parts;
    let _key = ScopedKey::require(&mut parts, &state, scopes::STATUS_READ).await?;
    match state.jobs.scan_request(&id).await {
        Some(rec) => Ok(Json(serde_json::to_value(rec)?)),
        None => Err(ApiError::NotFound("scan request")),
    }
}
