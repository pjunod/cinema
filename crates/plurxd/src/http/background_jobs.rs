//! Administrator observation and cancellation of durable background work.
use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use plurx_core::store::background_jobs::{
    BackgroundJob, CancelJob, EnqueueOutcome, JobKind, JobPayload, JobQuery, JobState, WaiterQuery,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListQuery {
    state: Option<JobState>,
    kind: Option<JobKind>,
    cursor: Option<String>,
}

fn valid_id(id: &str) -> Result<(), ApiError> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| ApiError::BadRequest("invalid job identity".into()))
}

fn summary(job: &BackgroundJob, now_ms: i64) -> Value {
    let kind = job
        .payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let file_id = job.payload.get("file_id").and_then(Value::as_i64);
    json!({
        "id": job.id, "kind": kind, "state": job.state, "priority": job.priority,
        "file_id": file_id.map(|id| id.to_string()),
        "owner_node_id": job.token.as_ref().map(|token| &token.node_id),
        "created_at_ms": job.created_at_ms, "updated_at_ms": job.updated_at_ms,
        "age_ms": now_ms.saturating_sub(job.created_at_ms).max(0),
        "not_before_ms": job.not_before_ms, "failed_attempts": job.failed_attempts,
        "attempt_limit": job.attempt_limit, "yield_count": job.yield_count,
        "abandoned_count": job.abandoned_count, "error_code": job.last_error_code,
        "payload_version": job.payload_version, "supported": job.supported_payload().is_ok(),
        "retry_deadline_ms": job.retry_deadline_ms,
        "retry_supported": matches!(job.state, JobState::Failed | JobState::Cancelled)
            && matches!(job.supported_payload(), Ok(JobPayload::TranscodePrepare { .. }
                | JobPayload::FragmentIndexBuild { .. } | JobPayload::ArtifactHydrate { .. })),
        "observation": "durable_state", "observed_at_ms": now_ms,
    })
}

pub async fn list(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if let Some(cursor) = &query.cursor {
        valid_id(cursor)?;
    }
    let now_ms = crate::state::clock_ms();
    let (page, counts, migration, repairs) = tokio::try_join!(
        state.store.list_jobs(JobQuery {
            state: query.state,
            kind: query.kind,
            after_id: query.cursor,
            limit: 100
        }),
        state.store.job_counts(now_ms),
        state.store.job_migration_status(),
        state.store.artifact_repairs(false),
    )?;
    let ids = page
        .jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<Vec<_>>();
    let labels = state.store.job_labels(&ids).await?;
    let jobs = page
        .jobs
        .iter()
        .map(|job| {
            let mut row = summary(job, now_ms);
            if let Some(label) = labels.iter().find(|label| label.id == job.id) {
                row["title"] = json!(label.title);
                row["library"] = json!(label.library);
            }
            row
        })
        .collect::<Vec<_>>();
    let repairs = repairs.iter().map(|repair| json!({
        "id":repair.id, "kind":repair.original_key.split(':').next().unwrap_or("artifact"),
        "target_node_id":repair.target_node_id, "phase":repair.phase, "job_id":repair.job_id,
        "age_ms":now_ms.saturating_sub(repair.created_at_ms).max(0), "updated_at_ms":repair.updated_at_ms,
    })).collect::<Vec<_>>();
    Ok(Json(json!({"jobs": jobs, "repairs":repairs,
        "counts": counts, "migration": migration, "next_cursor": page.next_after_id, "observed_at_ms": now_ms})))
}

pub async fn detail(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("background job"))?;
    let (attempts, waiters) = tokio::try_join!(
        state.store.job_attempts(&id),
        state.store.job_waiters(WaiterQuery {
            job_id: id.clone(),
            after: None,
            limit: 100,
        })
    )?;
    // Request identifiers, payloads, diagnostics, checkpoints, boot IDs and
    // publication references are not part of this operator response.
    Ok(Json(
        json!({"job": summary(&job, crate::state::clock_ms()), "attempts": attempts,
        "waiters": waiters.waiters.iter().map(|waiter| json!({"state": waiter.state,
            "consumer_kind": waiter.consumer_kind, "target_node_id": waiter.target_node_id,
            "priority": waiter.priority, "deadline_ms": waiter.deadline_ms,
            "updated_at_ms": waiter.updated_at_ms, "failed_attempts": waiter.failed_attempts,
            "attempt_limit": waiter.attempt_limit, "not_before_ms": waiter.not_before_ms,
            "retry_deadline_ms": waiter.retry_deadline_ms, "error_code": waiter.last_error_code})).collect::<Vec<_>>(),
        "more_waiters": waiters.next.is_some()}),
    ))
}

pub async fn cancel(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    let now_ms = crate::state::clock_ms();
    let job = match state
        .store
        .cancel_job(CancelJob {
            job_id: id.clone(),
            now_ms,
        })
        .await?
    {
        Some(job) => job,
        None => state
            .store
            .background_job(&id)
            .await?
            .ok_or(ApiError::NotFound("background job"))?,
    };
    Ok(Json(summary(&job, now_ms)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryBody {
    /// Retained by the client across transport retries of the same click.
    request_id: String,
}

pub async fn retry(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RetryBody>,
) -> Result<Json<Value>, ApiError> {
    use sha2::{Digest, Sha256};
    valid_id(&id)?;
    valid_id(&body.request_id)?;
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("background job"))?;
    if !matches!(job.state, JobState::Failed | JobState::Cancelled) {
        return Err(ApiError::Conflict(
            "only failed or cancelled work can be retried".into(),
        ));
    }
    let payload = job
        .supported_payload()
        .map_err(|_| ApiError::Conflict("this payload requires a compatible worker".into()))?;
    let now_ms = crate::state::clock_ms();
    if let JobPayload::FragmentIndexBuild {
        file_id,
        source_size,
        source_mtime,
        pipeline_digest,
        ..
    } = payload
    {
        // One deliberate request on this node; do not recreate another
        // consumer's cancelled delivery or renew all historical interests.
        let digest = Sha256::digest(format!(
            "fragment-retry:{}:{id}:{}",
            admin.id, body.request_id
        ));
        let request_id = uuid::Uuid::from_slice(&digest[..16])
            .expect("digest UUID")
            .to_string();
        if let Some(existing) = state.store.analysis_request(&request_id).await? {
            return Ok(Json(
                json!({"outcome":"existing", "analysis_request_id":existing.request_id, "state":existing.state}),
            ));
        }
        let file = state
            .store
            .get_file(file_id)
            .await?
            .filter(|file| file.size == source_size && file.mtime == source_mtime)
            .ok_or_else(|| {
                ApiError::Conflict(
                    "the source changed; request analysis of its current generation".into(),
                )
            })?;
        let engine = crate::ffmpeg::fragment_index_engine_digest().await;
        let identities = crate::state::fragment_index_video_identity_options(
            state.store.as_ref(),
            &file,
            state.transcode.dv_strippable(),
            state.transcode.dv_convert_enabled().await,
        )
        .await?;
        let mut matching = identities.into_iter().filter(|(video, _)| {
            crate::fragment_index_cluster::pipeline_digest(&file, &engine, *video)
                == pipeline_digest
        });
        let Some((_, video_identity)) = matching.next() else {
            return Err(ApiError::Conflict(
                "this pipeline changed; request a new analysis from the media detail page".into(),
            ));
        };
        if matching.next().is_some() {
            return Err(ApiError::Conflict(
                "the original video identity is ambiguous; request a specific analysis".into(),
            ));
        }
        let request = state
            .store
            .enqueue_analysis_request(&plurx_core::store::NewAnalysisRequest {
                request_id: request_id.clone(),
                file_id,
                source_size,
                source_mtime,
                component: "fragment_index".into(),
                pipeline_version: engine,
                video_identity,
                requested_generation: request_id.clone(),
                priority: "forced".into(),
                trigger: "admin".into(),
                force_rebuild: true,
                target_node_id: state.node_id.clone(),
                not_before_ms: now_ms,
                created_at_ms: now_ms,
            })
            .await?;
        if request.request_id != request_id {
            return Err(ApiError::Conflict(
                "another explicit generation is already pending; follow its analysis request"
                    .into(),
            ));
        }
        super::analysis::kick_analysis_queue(&state);
        tracing::info!(
            job_id = id,
            user_id = admin.id,
            successor_id = request_id,
            "durable fragment work explicitly retried"
        );
        return Ok(Json(
            json!({"outcome":"accepted", "analysis_request_id":request_id, "state":request.state}),
        ));
    }
    if !matches!(
        payload,
        JobPayload::TranscodePrepare { .. } | JobPayload::ArtifactHydrate { .. }
    ) {
        return Err(ApiError::Conflict(
            "this job requires its domain retry action".into(),
        ));
    }
    let outcome = state
        .store
        .retry_background_job(&id, &body.request_id, admin.id, now_ms)
        .await?
        .ok_or(ApiError::NotFound("background job"))?;
    match outcome {
        EnqueueOutcome::Accepted { .. } | EnqueueOutcome::Existing { .. } => {
            tracing::info!(
                job_id = id,
                user_id = admin.id,
                request_id = body.request_id,
                "durable work explicitly retried"
            );
            Ok(Json(serde_json::to_value(outcome).map_err(|error| {
                ApiError::BadRequest(error.to_string())
            })?))
        }
        EnqueueOutcome::SourceChanged => Err(ApiError::Conflict(
            "source changed or artifact retired; request new work from the file".into(),
        )),
        EnqueueOutcome::QueueFull => Err(ApiError::Conflict(
            "durable queue is full; retry later".into(),
        )),
        _ => Err(ApiError::Conflict(
            "retry conflicts with the current work or request identity".into(),
        )),
    }
}

/// Administrator-only: source paths are deliberately excluded from job lists.
pub async fn storage_domains(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let (mappings, libraries) =
        tokio::try_join!(state.store.storage_domains(), state.store.list_libraries())?;
    Ok(Json(json!({"mappings": mappings, "libraries": libraries,
        "slots_per_domain": 2, "provider_slots": 2})))
}

pub async fn replace_storage_domains(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(mappings): Json<Vec<plurx_core::store::background_jobs_resources::StorageDomainMapping>>,
) -> Result<Json<Value>, ApiError> {
    plurx_core::store::background_jobs_resources::validate(&mappings)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    if !state
        .store
        .replace_storage_domains(mappings, crate::state::clock_ms())
        .await?
    {
        return Err(ApiError::Conflict("Storage identities cannot change while background work owns live reservations, or when a root no longer exists. Retry after the active jobs finish and reload the library roots.".into()));
    }
    Ok(Json(json!({"saved": true})))
}
