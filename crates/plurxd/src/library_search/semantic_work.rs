//! Durable item computation; query embedding remains local and ephemeral.
use super::*;
use plurx_core::store::background_jobs::{
    CandidateCursor, CandidateQuery, ClaimJob, EnqueueJob, JobKind, JobPayload, JobRequest,
    JobSettlement,
};
use plurx_core::store::background_jobs_embeddings::SharedEmbedding;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
pub(super) async fn enqueue(
    state: &AppState,
    item_id: i64,
    content: &str,
    predecessor: Option<&str>,
) -> Result<()> {
    let payload = JobPayload::SemanticEmbedding {
        item_id,
        content_digest: content.into(),
        model_digest: model_identity().digest(),
    };
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&payload)?));
    let request_id = hex::encode(Sha256::digest(format!(
        "{digest}:{}",
        predecessor.unwrap_or("initial")
    )));
    let now = now_ms();
    state
        .store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("embedding:{digest}"),
            priority: 3,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "semantic".into(),
                request_id,
                request_digest: digest,
                consumer_kind: "semantic".into(),
                consumer_ref: item_id.to_string(),
                target_node_id: None,
                deadline_ms: Some(now.saturating_add(86_400_000)),
                retain_identity: false,
            },
        })
        .await?;
    Ok(())
}

pub(super) async fn run(
    state: &AppState,
    boot: &str,
    cursor: &mut Option<CandidateCursor>,
    shutdown: &CancellationToken,
) -> Result<bool> {
    let authority = state.jobs.execution_authority();
    if shutdown.is_cancelled()
        || !ENABLED.load(Ordering::Acquire)
        || !authority.may_execute_job(JobKind::SemanticEmbedding).await
    {
        return Ok(false);
    }
    let loaded = runtime().try_lock().is_ok_and(|r| r.encoder.is_some());
    if !loaded {
        return Ok(false);
    }
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(false);
    };
    let page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::SemanticEmbedding],
            after: cursor.take(),
            now_ms: now_ms(),
            limit: 64,
        })
        .await?;
    *cursor = page.next;
    let identity = model_identity();
    for candidate in page.jobs {
        let Ok(JobPayload::SemanticEmbedding {
            item_id,
            content_digest,
            model_digest,
        }) = candidate.supported_payload()
        else {
            continue;
        };
        if model_digest != identity.digest() {
            continue;
        }
        if shutdown.is_cancelled()
            || !ENABLED.load(Ordering::Acquire)
            || !state.transcode.fragment_worker_idle(&admission)
        {
            return Ok(false);
        }
        let now = now_ms();
        let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
            state.store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: boot.into(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SemanticEmbedding,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            },
        )
        .await?
        else {
            continue;
        };
        let active = crate::background_jobs::ActiveBackgroundJob::start(
            Arc::clone(&state.store),
            authority,
            job.token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("embedding claim missing token"))?,
            deadline,
            JobKind::SemanticEmbedding,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let result: Result<bool> = async {
            let entries = state.store.classification_page(item_id.saturating_sub(1), 1).await?;
            let Some(entry) = entries.first().filter(|entry| entry.input().is_ok_and(|input| input.id == item_id)
                && source_key(entry) == content_digest) else {
                fence.settle(JobSettlement::Stop { error_code: "embedding_source_changed".into() }).await?;
                return Ok(true);
            };
            let text = embedding_text(entry)?;
            let source_json = entry.source_json.clone();
            let revision = entry.record.as_ref().map_or(0, |record| record.revision);
            let r = runtime();
            let cancelled = cancel.clone();
            let mut task = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
                let r = r.lock().map_err(|_| anyhow::anyhow!("model lock"))?;
                if cancelled.is_cancelled() || !ENABLED.load(Ordering::Acquire) { bail!("embedding cancelled"); }
                r.encoder.as_ref().ok_or_else(|| anyhow::anyhow!("encoder unloaded"))?.embed(&text)
            });
            let vector = loop {
                tokio::select! {
                    result = &mut task => break result??,
                    () = tokio::time::sleep(Duration::from_millis(250)) => {
                        if shutdown.is_cancelled() || !ENABLED.load(Ordering::Acquire)
                            || !state.transcode.fragment_worker_idle(&admission) { cancel.cancel(); }
                    }
                }
            };
            // The CPU task is joined before releasing physical admission, even
            // when the lease or consumer disappeared during inference.
            if cancel.is_cancelled() || shutdown.is_cancelled() || !ENABLED.load(Ordering::Acquire) { return Ok(false); }
            let artifact = SharedEmbedding { item_id, content_digest, model: identity,
                vector_sha256: SharedEmbedding::vector_digest(&vector), vector };
            Ok(fence.publish_embedding(artifact, source_json, revision).await?)
        }.await;
        if !matches!(result, Ok(true)) {
            let settlement = if cancel.is_cancelled()
                || shutdown.is_cancelled()
                || !ENABLED.load(Ordering::Acquire)
            {
                JobSettlement::Yield {
                    not_before_ms: now_ms().saturating_add(5000),
                    checkpoint: None,
                }
            } else {
                if let Err(error) = result {
                    tracing::warn!(job = job.id, %error, "semantic embedding failed");
                }
                JobSettlement::Retry {
                    error_code: "embedding_unavailable".into(),
                    not_before_ms: now_ms().saturating_add(crate::background_jobs::retry_delay_ms(
                        &job.id,
                        job.failed_attempts,
                    )),
                }
            };
            if let Err(error) = fence.settle(settlement).await {
                tracing::warn!(%error, "embedding settlement unavailable");
            }
        }
        active.finish().await;
        return Ok(true);
    }
    Ok(false)
}
