//! Admin controls for permanent Dolby Vision Profile 7 conversion.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::store::{QueueDvConversionOutcome, DV_CONVERSION_LEDGER_READ_MAX};
use serde::Deserialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::{AppState, DvDiskMode};

fn queue_error(error: plurx_core::error::StoreError) -> ApiError {
    let message = error.to_string();
    if message.contains("library.dv_disk_convert unavailable") {
        ApiError::ServiceUnavailable(message)
    } else {
        ApiError::from(error)
    }
}

#[derive(Default, Deserialize)]
pub struct StatusQuery {
    library_id: Option<i64>,
    file_ids: Option<String>,
}

fn parse_file_ids(raw: &str) -> Result<Vec<i64>, ApiError> {
    let mut ids = std::collections::BTreeSet::new();
    for value in raw.split(',').filter(|value| !value.trim().is_empty()) {
        let id = value
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                ApiError::BadRequest("file_ids must contain positive integers".to_owned())
            })?;
        ids.insert(id);
        if ids.len() > DV_CONVERSION_LEDGER_READ_MAX {
            return Err(ApiError::BadRequest(format!(
                "file_ids accepts at most {DV_CONVERSION_LEDGER_READ_MAX} ids"
            )));
        }
    }
    Ok(ids.into_iter().collect())
}

pub async fn status(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(raw) = query.file_ids.as_deref() {
        let ids = parse_file_ids(raw)?;
        let conversions = state.store.dv_conversions_for_files(&ids).await?;
        let eligibility = state
            .store
            .dv_conversion_eligibility_for_files(&ids)
            .await?;
        return Ok(Json(serde_json::json!({
            "conversions_by_file": conversions.into_iter().map(|row| {
                (row.file_id.to_string(), row)
            }).collect::<std::collections::BTreeMap<_, _>>(),
            "eligible_by_file": eligibility.into_iter().map(|(file_id, eligible)| {
                (file_id.to_string(), eligible)
            }).collect::<std::collections::BTreeMap<_, _>>(),
            "capabilities": state.jobs.dv_disk_capabilities(),
        })));
    }
    let progress_snapshot = state.store.dv_conversion_progress_snapshot().await?;
    let progress = query.library_id.map_or_else(
        || progress_snapshot.global.clone(),
        |library_id| {
            progress_snapshot
                .by_library
                .get(&library_id)
                .cloned()
                .unwrap_or_default()
        },
    );
    let progress_by_library = query.library_id.is_none().then(|| {
        progress_snapshot
            .by_library
            .into_iter()
            .map(|(id, progress)| (id.to_string(), progress))
            .collect::<std::collections::BTreeMap<_, _>>()
    });
    let (modes, keep_original, parallel) = state.jobs.dv_disk_settings_snapshot().await?;
    Ok(Json(serde_json::json!({
        "capabilities": state.jobs.dv_disk_capabilities(),
        "library_modes": modes.into_iter().map(|(id, mode)| {
            (id.to_string(), mode.as_str())
        }).collect::<std::collections::BTreeMap<_, _>>(),
        "keep_original": keep_original,
        "parallel": parallel,
        "progress": progress,
        "progress_by_library": progress_by_library.unwrap_or_default(),
    })))
}

pub async fn file_status(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let eligible = file
        .container
        .as_deref()
        .is_some_and(|container| container.eq_ignore_ascii_case("mkv"))
        && file.dolby_vision.profile == Some(7)
        && matches!(file.dolby_vision.bl_compat_id, Some(1 | 6))
        && file.dolby_vision.el_present == Some(true)
        && file.dolby_vision.rpu_present == Some(true);
    Ok(Json(serde_json::json!({
        "file_id": file_id.to_string(),
        "eligible": eligible,
        "dolby_vision": file.dolby_vision,
        "conversion": state.store.dv_conversion(file_id).await?,
        "capabilities": state.jobs.dv_disk_capabilities(),
    })))
}

pub async fn queue_file(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let outcome = state
        .jobs
        .queue_dv_file(file_id)
        .await
        .map_err(queue_error)?;
    match outcome {
        QueueDvConversionOutcome::Queued(row) => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "queued": true, "conversion": row })),
        )),
        QueueDvConversionOutcome::AlreadyActive(row) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "queued": false, "conversion": row })),
        )),
        QueueDvConversionOutcome::AlreadyCommitted(row) => Err(ApiError::Conflict(format!(
            "file {} was already committed and cannot be re-queued",
            row.file_id
        ))),
        QueueDvConversionOutcome::Ineligible(reason) => {
            Err(ApiError::Unprocessable(serde_json::json!({
                "error": reason,
                "file_id": file_id.to_string(),
            })))
        }
        QueueDvConversionOutcome::FileMissing => Err(ApiError::NotFound("file")),
    }
}

#[derive(Default, Deserialize)]
pub struct QueueLibraryBody {
    #[serde(default = "default_retry_failed")]
    retry_failed: bool,
}

fn default_retry_failed() -> bool {
    true
}

pub async fn queue_library(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(library_id): Path<i64>,
    body: Option<Json<QueueLibraryBody>>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    state
        .store
        .get_library(library_id)
        .await?
        .ok_or(ApiError::NotFound("library"))?;
    let retry_failed = body.map(|Json(body)| body.retry_failed).unwrap_or(true);
    let batch = state
        .jobs
        .queue_dv_library(library_id, retry_failed)
        .await
        .map_err(queue_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "library_id": library_id.to_string(),
            "queued": batch.queued,
            "saturated": batch.saturated,
        })),
    ))
}

#[derive(Deserialize)]
pub struct ModeBody {
    mode: String,
}

pub async fn set_mode(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(library_id): Path<i64>,
    Json(body): Json<ModeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mode = DvDiskMode::parse(body.mode.trim())
        .ok_or_else(|| ApiError::BadRequest("mode must be off, manual, or auto".to_owned()))?;
    state
        .store
        .get_library(library_id)
        .await?
        .ok_or(ApiError::NotFound("library"))?;
    state
        .jobs
        .set_dv_disk_mode(library_id, mode)
        .await
        .map_err(queue_error)?;
    Ok(Json(serde_json::json!({
        "library_id": library_id.to_string(),
        "mode": mode.as_str(),
    })))
}
