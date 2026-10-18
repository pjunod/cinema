//! One bounded owned translation operation at a time on the serving worker.
use super::{artifacts, profile, runtime, AppState, StoreError, TranslationQuery, TranslationRead};
use crate::background_jobs::{ActiveBackgroundJob, JobFence};
use plurx_core::store::{
    background_jobs::{
        CandidateQuery, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest,
        JobSettlement,
    },
    subtitle_translation::{RangeRecipe, SourceRevision, TranslationProfile},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

async fn read(state: &AppState, query: TranslationQuery) -> Result<Vec<Value>, StoreError> {
    tokio::time::timeout(
        Duration::from_secs(2),
        state.store.read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: crate::state::clock_ms(),
            query,
        }),
    )
    .await
    .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
}

fn number(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}
fn demand_wants(d: &Value, core: i64, width: i64) -> bool {
    let current = number(&d["position_ms"]).unwrap_or(0) / width;
    let ahead = (d["playback_rate"].as_f64().unwrap_or(1.0) as i64 + 1).min(2);
    core >= current && core <= current + ahead
}
async fn interested(state: &AppState, source: &JobPayload) -> Result<bool, StoreError> {
    let (op, epoch, core) = match source {
        JobPayload::SubtitleTranslationPrepare {
            operation_id,
            operation_epoch,
            ..
        } => (operation_id, *operation_epoch, None),
        JobPayload::SubtitleTranslationRange { recipe, .. } => (
            &recipe.operation_id,
            recipe.operation_epoch,
            Some((recipe.core_index, i64::from(recipe.profile.core_ms))),
        ),
        _ => return Ok(true),
    };
    let demands = read(
        state,
        TranslationQuery::LiveDemands {
            operation_id: op.clone(),
            operation_epoch: epoch,
        },
    )
    .await?;
    Ok(demands
        .iter()
        .any(|d| core.is_none_or(|(index, width)| demand_wants(d, index, width))))
}

/// Whether this node's current boot holds the source binding the store
/// requires before it accepts this operation's published cues.
async fn bound_this_boot(
    state: &AppState,
    asset_id: &str,
    operation_id: &str,
    epoch: i64,
) -> Result<bool, StoreError> {
    Ok(read(
        state,
        TranslationQuery::SourceBinding {
            asset_id: asset_id.into(),
            node_id: state.node_id.clone(),
        },
    )
    .await?
    .iter()
    .any(|b| {
        b["operation_id"] == operation_id
            && number(&b["operation_epoch"]) == Some(epoch)
            && b["boot_id"] == state.subtitle_models.boot_id.as_str()
    }))
}

/// Enqueue at most one missing core for operations a viewer currently wants.
/// Returns whether any operation had live demand.
async fn schedule(state: &AppState) -> Result<bool, StoreError> {
    let after = state
        .subtitle_models
        .translation
        .operation_cursor
        .read()
        .await
        .clone();
    let mut ops = read(
        state,
        TranslationQuery::WorkerOperations {
            node_id: state.node_id.clone(),
            after,
        },
    )
    .await?;
    let next = (ops.len() > 32).then(|| {
        ops[31]["operation_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    });
    *state
        .subtitle_models
        .translation
        .operation_cursor
        .write()
        .await = next;
    ops.truncate(32);
    let demanded = !ops.is_empty();
    if !ops.is_empty() {
        let turn = state
            .subtitle_models
            .translation
            .turn
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let len = ops.len();
        ops.rotate_left(turn % len);
    }

    for op in ops {
        let Some(asset) = op["asset_id"].as_str() else {
            continue;
        };
        let operation_id = op["operation_id"]
            .as_str()
            .ok_or_else(|| StoreError::Task("translation operation identity".into()))?
            .to_owned();
        let epoch = number(&op["operation_epoch"])
            .ok_or_else(|| StoreError::Task("translation epoch".into()))?;
        let profile: TranslationProfile = serde_json::from_value(op["profile"].clone())
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?;
        if profile != super::profile::selected_profile() {
            continue;
        }
        let source: SourceRevision = serde_json::from_value(op["source"].clone())
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?;
        let Some(file) =
            tokio::time::timeout(Duration::from_secs(2), state.store.get_file(source.file_id))
                .await
                .map_err(|_| StoreError::Task("worker_source_unavailable".into()))??
        else {
            continue;
        };
        let Some(held) = state.subtitle_models.translation.cached(&file).await else {
            continue;
        };
        if !bound_this_boot(state, asset, &operation_id, epoch).await? {
            continue;
        }
        let duration = number(&op["duration_ms"]).unwrap_or(0);
        let demands = read(
            state,
            TranslationQuery::LiveDemands {
                operation_id: operation_id.clone(),
                operation_epoch: epoch,
            },
        )
        .await?;
        // Rotate the first viewer to prevent a stable UUID ordering starving a
        // second viewer. All viewers may keep a shared core alive.
        let turn = state
            .subtitle_models
            .translation
            .turn
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for offset in 0..demands.len() {
            let d = &demands[(offset + turn) % demands.len()];
            let current = number(&d["position_ms"]).unwrap_or(0) / i64::from(profile.core_ms);
            for index in
                current..=current + (d["playback_rate"].as_f64().unwrap_or(1.0) as i64 + 1).min(2)
            {
                if index * i64::from(profile.core_ms) >= duration {
                    break;
                }
                let prior = read(
                    state,
                    TranslationQuery::CoreState {
                        operation_id: operation_id.clone(),
                        operation_epoch: epoch,
                        core_index: index,
                    },
                )
                .await?;
                if prior.first().is_some_and(|r| {
                    r["accepted"] == true
                        || r["state"]
                            .as_str()
                            .is_some_and(|s| ["queued", "running", "failed"].contains(&s))
                }) {
                    continue;
                }
                let recipe = RangeRecipe {
                    asset_id: asset.into(),
                    operation_id: operation_id.clone(),
                    operation_epoch: epoch,
                    source: source.clone(),
                    source_sha256: held.sha256().into(),
                    profile: profile.clone(),
                    audio_index: 0,
                    spoken_language: op["spoken_language"].as_str().unwrap_or_default().into(),
                    duration_ms: duration,
                    core_index: index,
                };
                recipe.validate()?;
                let now = crate::state::clock_ms();
                let id = uuid::Uuid::new_v4().to_string();
                let digest = plurx_core::store::subtitle_translation::digest(
                    &serde_json::to_value(&recipe)
                        .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?,
                )?;
                state
                    .store
                    .enqueue_job(EnqueueJob {
                        id: id.clone(),
                        payload: JobPayload::SubtitleTranslationRange {
                            file_id: source.file_id,
                            recipe,
                            target_node_id: state.node_id.clone(),
                        },
                        dedupe_key: format!(
                            "translation-range:{operation_id}:{epoch}:{index}:{}",
                            prior
                                .first()
                                .and_then(|r| number(&r["range_revision"]))
                                .unwrap_or(0)
                        ),
                        priority: 1,
                        not_before_ms: now,
                        now_ms: now,
                        request: JobRequest {
                            scope: "subtitle-translation-range".into(),
                            request_id: id,
                            request_digest: digest.clone(),
                            consumer_kind: "subtitle_translation_range".into(),
                            consumer_ref: digest,
                            target_node_id: Some(state.node_id.clone()),
                            deadline_ms: None,
                            retain_identity: false,
                        },
                    })
                    .await?;
                // Bound scheduling and prioritize the first absent core.
                return Ok(true);
            }
        }
    }
    Ok(demanded)
}

async fn execute(
    state: &AppState,
    source: &JobPayload,
    fence: &JobFence,
    cancel: &CancellationToken,
    admission: Option<&crate::transcode::FragmentAdmission>,
    expected_model_identity: Option<&str>,
) -> Result<bool, StoreError> {
    if let JobPayload::SubtitleModelOperation { .. } = source {
        let engine = artifacts::inspect_engine()
            .await
            .map_err(StoreError::Task)?;
        let qualified = super::installer::execute(state, source, &engine, fence, cancel).await?;
        if expected_model_identity
            .is_some_and(|identity| identity != qualified.receipt.model_file_identity)
        {
            return Err(StoreError::Task("automatic_verification_obsolete".into()));
        }
        if cancel.is_cancelled() {
            return Err(StoreError::Task("worker_interrupted".into()));
        }
        return fence
            .publish_translation_model(source.clone(), qualified.receipt, qualified.translation)
            .await;
    }
    let admission =
        admission.ok_or_else(|| StoreError::Task("waiting_for_playback_capacity".into()))?;
    let (revision, selected) = match source {
        JobPayload::SubtitleTranslationPrepare {
            file_id,
            source_size,
            source_mtime,
            profile,
            ..
        } => (
            SourceRevision {
                file_id: *file_id,
                source_size: *source_size,
                source_mtime: *source_mtime,
            },
            profile,
        ),
        JobPayload::SubtitleTranslationRange { recipe, .. } => {
            (recipe.source.clone(), &recipe.profile)
        }
        _ => return Ok(false),
    };
    if selected != &profile::selected_profile() {
        return Err(StoreError::Task("translation_profile_unavailable".into()));
    }
    let file = tokio::time::timeout(
        Duration::from_secs(2),
        state.store.get_file(revision.file_id),
    )
    .await
    .map_err(|_| StoreError::Task("worker_source_unavailable".into()))??
    .filter(|f| f.size == revision.source_size && f.mtime == revision.source_mtime)
    .ok_or_else(|| StoreError::Task("source_changed".into()))?;
    super::file_capability(state, &file)
        .await
        .map_err(|e| StoreError::Task(e.into()))?;
    let held = match state.subtitle_models.translation.cached(&file).await {
        Some(held) => held,
        // Preparation alone verifies the source and records this boot's
        // binding. A range never re-verifies on its own: its publication
        // would be refused without that binding.
        None if matches!(source, JobPayload::SubtitleTranslationRange { .. }) => {
            return Err(StoreError::Task("source_unverified".into()));
        }
        None => {
            let verified = Arc::new(
                runtime::verify_source(state, &file, &revision, fence, admission, cancel).await?,
            );
            state
                .subtitle_models
                .translation
                .remember(verified.clone())
                .await;
            verified
        }
    };
    if !interested(state, source).await? || cancel.is_cancelled() {
        return Err(StoreError::Task("demand_expired".into()));
    }
    match source {
        JobPayload::SubtitleTranslationPrepare { .. } => {
            fence
                .bind_translation_source(
                    source.clone(),
                    held.sha256().into(),
                    artifacts::identity_key(held.identity()),
                )
                .await
        }
        JobPayload::SubtitleTranslationRange { recipe, .. } => {
            if recipe.source_sha256 != held.sha256() {
                return Err(StoreError::Task("source_changed".into()));
            }
            if !bound_this_boot(
                state,
                &recipe.asset_id,
                &recipe.operation_id,
                recipe.operation_epoch,
            )
            .await?
            {
                return Err(StoreError::Task("source_unverified".into()));
            }
            let engine = artifacts::inspect_engine()
                .await
                .map_err(StoreError::Task)?;
            let spec = profile::model_spec();
            let hash =
                artifacts::open_model_cancellable(&state.subtitle_models.root, &spec, cancel);
            tokio::pin!(hash);
            let model = tokio::select! {result=&mut hash=>result.map_err(StoreError::Task)?,()=crate::subtitle_transcription::watch_capacity(&state.transcode,admission)=>{cancel.cancel();let _=hash.await;return Err(StoreError::Task("waiting_for_playback_capacity".into()));}};
            // The current receipt must match the exact model identity opened
            // AFTER admission; metadata observed before a claim is insufficient.
            if !super::workers_public(state)
                .await
                .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
                .iter()
                .any(|w| {
                    w["capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.iter().any(|c| c["status"] == "verified"))
                })
            {
                return Err(StoreError::Task("translation_unverified".into()));
            }
            if artifacts::model_identity(&state.subtitle_models.root, &spec)
                .await
                .map_err(StoreError::Task)?
                != model.identity
            {
                return Err(StoreError::Task("artifact_changed".into()));
            }
            let segment = runtime::translate_range(
                state, &file, &held, recipe, &engine, &model, fence, admission, cancel,
            )
            .await?;
            if cancel.is_cancelled() {
                return Err(StoreError::Task("demand_expired".into()));
            }
            if state
                .subtitle_models
                .translation
                .cached(&file)
                .await
                .is_none()
            {
                return Err(StoreError::Task("source_changed".into()));
            }
            fence
                .publish_translation_segment(source.clone(), segment)
                .await
        }
        _ => Ok(false),
    }
}

fn memoizes_automatic(outcome: &EnqueueOutcome) -> bool {
    matches!(
        outcome,
        EnqueueOutcome::Accepted { .. } | EnqueueOutcome::Existing { .. }
    )
}
fn accepted_capability(
    receipt: &super::Capability,
    selected: &TranslationProfile,
    sha: &str,
    identity: &str,
) -> bool {
    receipt.profile == *selected
        && receipt.profile_id == selected.id().unwrap_or_default()
        && receipt.self_test_sha256 == profile::SELF_TEST_SHA256
        && receipt.model.model_id == selected.model_id
        && receipt.model.model_sha256 == sha
        && receipt.model.model_file_identity == identity
}

async fn automatic_verification(state: &AppState) -> Result<(), StoreError> {
    let now = crate::state::clock_ms();
    if now
        < state
            .subtitle_models
            .translation
            .automatic_next
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(());
    }
    state.subtitle_models.translation.automatic_next.store(
        now.saturating_add(15000),
        std::sync::atomic::Ordering::Relaxed,
    );

    let selected = profile::selected_profile();
    let spec = profile::model_spec();
    let profile_id = selected.id()?;
    if state
        .subtitle_models
        .translation
        .failures
        .read()
        .await
        .contains_key(&profile_id)
    {
        return Ok(());
    }
    let Ok(identity) = artifacts::model_identity(&state.subtitle_models.root, &spec).await else {
        return Ok(());
    };
    let Ok(engine) = artifacts::inspect_engine().await else {
        return Ok(());
    };
    let receipts = read(
        state,
        TranslationQuery::Capabilities {
            node_id: state.node_id.clone(),
        },
    )
    .await?;
    let accepted = receipts
        .into_iter()
        .filter_map(|r| serde_json::from_value::<super::Capability>(r).ok())
        .any(|r| {
            accepted_capability(
                &r,
                &selected,
                &spec.sha256,
                &artifacts::identity_key(identity),
            )
        });
    if !accepted {
        return Ok(());
    }
    if super::workers_public(state)
        .await
        .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
        .iter()
        .any(|w| {
            w["capabilities"]
                .as_array()
                .is_some_and(|c| c.iter().any(|v| v["status"] == "verified"))
        })
    {
        return Ok(());
    }
    let key = plurx_core::store::subtitle_translation::digest(&serde_json::json!([
        state.node_id,
        state.subtitle_models.boot_id,
        profile_id,
        artifacts::identity_key(identity),
        artifacts::identity_key(engine.identity)
    ]))?;
    if state
        .subtitle_models
        .translation
        .automatic_keys
        .read()
        .await
        .contains(&key)
    {
        return Ok(());
    }
    let bytes = hex::decode(&key[..32]).map_err(|e| StoreError::Task(e.to_string()))?;
    let request_id = uuid::Uuid::from_slice(&bytes)
        .map_err(|e| StoreError::Task(e.to_string()))?
        .to_string();
    let now = crate::state::clock_ms();
    let outcome=state.store.enqueue_job(EnqueueJob{id:uuid::Uuid::new_v4().to_string(),payload:JobPayload::SubtitleModelOperation{translation_profile:Some(selected),action:plurx_core::store::background_jobs_transcription::ModelAction::TranslationVerify,model_id:spec.id,catalog_revision:crate::subtitle_models::catalog().revision.clone(),model_sha256:spec.sha256,adapter_contract:engine.adapter_contract,target_node_id:state.node_id.clone()},dedupe_key:format!("translation-auto:{key}"),priority:0,not_before_ms:now,now_ms:now,request:JobRequest{scope:"translation-auto".into(),request_id,request_digest:key.clone(),consumer_kind:"subtitle_model_operation".into(),consumer_ref:key.clone(),target_node_id:Some(state.node_id.clone()),deadline_ms:None,retain_identity:true}}).await?;
    if memoizes_automatic(&outcome) {
        let mut attempted = state
            .subtitle_models
            .translation
            .automatic_keys
            .write()
            .await;
        attempted.push_back(key);
        while attempted.len() > 16 {
            attempted.pop_front();
        }
    }
    Ok(())
}

/// One translation pass. Returns whether translation has live work (a
/// demanded operation or a runnable job), which keeps the loop responsive.
pub(crate) async fn worker_pass(
    state: &AppState,
    shutdown: &CancellationToken,
) -> Result<bool, StoreError> {
    let now = crate::state::clock_ms();
    let previous = state
        .subtitle_models
        .translation
        .maintenance_at
        .load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(previous) >= 60000 {
        state.store.maintain_translation_cache(now).await?;
        state
            .subtitle_models
            .translation
            .maintenance_at
            .store(now, std::sync::atomic::Ordering::Relaxed);
    }
    automatic_verification(state).await?;
    let demanded = schedule(state).await?;
    let authority = state.jobs.execution_authority();
    let after = state
        .subtitle_models
        .translation
        .candidate_cursor
        .read()
        .await
        .clone();
    let candidates = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![
                JobKind::SubtitleModelOperation,
                JobKind::SubtitleTranslationPrepare,
                JobKind::SubtitleTranslationRange,
            ],
            after,
            now_ms: crate::state::clock_ms(),
            limit: 32,
        })
        .await?;
    *state
        .subtitle_models
        .translation
        .candidate_cursor
        .write()
        .await = candidates.next;
    for candidate in candidates.jobs {
        let source = candidate.supported_payload()?;
        let model = matches!(&source,JobPayload::SubtitleModelOperation{action,..} if action.is_translation());
        if matches!(&source, JobPayload::SubtitleModelOperation { .. }) && !model {
            continue;
        }
        if shutdown.is_cancelled() || !authority.may_execute_job(source.kind()).await {
            continue;
        }
        if !model && !interested(state, &source).await? {
            state
                .store
                .cancel_job(plurx_core::store::background_jobs::CancelJob {
                    job_id: candidate.id.clone(),
                    now_ms: crate::state::clock_ms(),
                })
                .await?;
            continue;
        }
        let expected_model_identity = if model
            && candidate.dedupe_key.starts_with("translation-auto:")
        {
            if state
                .subtitle_models
                .translation
                .failures
                .read()
                .await
                .contains_key(&profile::selected_profile().id()?)
            {
                state
                    .store
                    .cancel_job(plurx_core::store::background_jobs::CancelJob {
                        job_id: candidate.id.clone(),
                        now_ms: crate::state::clock_ms(),
                    })
                    .await?;
                continue;
            }
            let spec = profile::model_spec();
            let Ok(identity) = artifacts::model_identity(&state.subtitle_models.root, &spec).await
            else {
                continue;
            };
            let Ok(engine) = artifacts::inspect_engine().await else {
                continue;
            };
            let accepted = read(
                state,
                TranslationQuery::Capabilities {
                    node_id: state.node_id.clone(),
                },
            )
            .await?
            .into_iter()
            .filter_map(|r| serde_json::from_value::<super::Capability>(r).ok())
            .any(|r| {
                accepted_capability(
                    &r,
                    &profile::selected_profile(),
                    &spec.sha256,
                    &artifacts::identity_key(identity),
                )
            });
            if !accepted {
                state
                    .store
                    .cancel_job(plurx_core::store::background_jobs::CancelJob {
                        job_id: candidate.id.clone(),
                        now_ms: crate::state::clock_ms(),
                    })
                    .await?;
                continue;
            }
            let key = plurx_core::store::subtitle_translation::digest(&serde_json::json!([
                state.node_id,
                state.subtitle_models.boot_id,
                profile::selected_profile().id()?,
                artifacts::identity_key(identity),
                artifacts::identity_key(engine.identity)
            ]))?;
            if candidate.dedupe_key != format!("translation-auto:{key}") {
                state
                    .store
                    .cancel_job(plurx_core::store::background_jobs::CancelJob {
                        job_id: candidate.id.clone(),
                        now_ms: crate::state::clock_ms(),
                    })
                    .await?;
                continue;
            }
            Some(artifacts::identity_key(identity))
        } else {
            None
        };
        let admission = if model {
            None
        } else {
            if !super::workers_public(state)
                .await
                .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
                .iter()
                .any(|w| {
                    w["capabilities"]
                        .as_array()
                        .is_some_and(|c| c.iter().any(|v| v["status"] == "verified"))
                })
            {
                continue;
            }
            match state.transcode.admit_fragment().await {
                Some(a) if state.transcode.fragment_worker_idle(&a) => Some(a),
                _ => continue,
            }
        };
        let now = crate::state::clock_ms();
        let kind = source.kind();
        let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
            state.store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: state.subtitle_models.boot_id.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            },
        )
        .await?
        else {
            continue;
        };
        let active = ActiveBackgroundJob::start(
            state.store.clone(),
            authority.clone(),
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("translation claim token missing".into()))?,
            deadline,
            kind,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let operation = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            execute(
                state,
                &source,
                &fence,
                &cancel,
                admission.as_ref(),
                expected_model_identity.as_deref(),
            ),
        );
        tokio::pin!(operation);
        let result = loop {
            tokio::select! {
                result=&mut operation=>break result,
                ()=shutdown.cancelled()=>{cancel.cancel();break operation.await;},
                ()=tokio::time::sleep(Duration::from_millis(250))=>{
                    match tokio::time::timeout(Duration::from_secs(2),interested(state,&source)).await{Ok(Ok(true))=>{},Ok(Ok(false))|Ok(Err(_))|Err(_)=>{cancel.cancel();let _=operation.await;break Err(StoreError::Task("demand_expired".into()));}}
                }
            }
        };
        match result {
            Ok(true) => {
                if model {
                    state
                        .subtitle_models
                        .translation
                        .failures
                        .write()
                        .await
                        .remove(&profile::selected_profile().id()?);
                }
            }
            Ok(false) => {
                let _ = fence
                    .settle(JobSettlement::Stop {
                        error_code: "translation_publication_refused".into(),
                    })
                    .await;
            }
            Err(error) => {
                let message = error.to_string();
                if message.contains("demand_expired")
                    || message.contains("automatic_verification_obsolete")
                {
                    let _ = fence
                        .settle(JobSettlement::Stop {
                            error_code: "demand_expired".into(),
                        })
                        .await;
                } else if message.contains("source_unverified") {
                    // Not a range failure: this boot has no source proof for
                    // the operation. A fresh translation request prepares it.
                    let _ = fence
                        .settle(JobSettlement::Stop {
                            error_code: "source_unverified".into(),
                        })
                        .await;
                } else if cancel.is_cancelled()
                    || shutdown.is_cancelled()
                    || message.contains("playback_capacity")
                    || message.contains("worker_interrupted")
                {
                    let _ = fence
                        .settle(JobSettlement::Yield {
                            error_code: Some(
                                if message.contains("playback") {
                                    "waiting_for_playback_capacity"
                                } else {
                                    "worker_interrupted"
                                }
                                .into(),
                            ),
                            checkpoint: None,
                            not_before_ms: crate::state::clock_ms() + 1000,
                        })
                        .await;
                } else {
                    let reason = safe_reason(&message);
                    if model {
                        state
                            .subtitle_models
                            .translation
                            .failures
                            .write()
                            .await
                            .insert(profile::selected_profile().id()?, reason.into());
                    }
                    let _ = fence
                        .settle(JobSettlement::Fail {
                            error_code: reason.into(),
                        })
                        .await;
                }
            }
        }
        active.finish().await;
        return Ok(true);
    }
    Ok(demanded)
}
fn safe_reason(message: &str) -> &'static str {
    if message.contains("engine") {
        return "engine_unavailable";
    }
    if message.contains("artifact_digest") || message.contains("artifact_length") {
        return "model_verification_failed";
    }

    [
        "source_changed",
        "source_clock_unsupported",
        "worker_source_unavailable",
        "translation_profile_unavailable",
        "translation_unverified",
        "decode_failed",
        "no_speech_detected",
        "inference_failed",
        "inference_timeout",
        "output_invalid",
        "output_too_large",
        "engine_unavailable",
        "model_verification_failed",
        "artifact_changed",
    ]
    .into_iter()
    .find(|code| message.ends_with(code))
    .unwrap_or("translation_failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_demand_window_preserves_peer_interest_and_caps_total_cores() {
        let a = serde_json::json!({"position_ms":0,"playback_rate":4.0});
        let b = serde_json::json!({"position_ms":120000,"playback_rate":1.0});
        assert!(demand_wants(&a, 2, 30000));
        assert!(!demand_wants(&a, 3, 30000));
        assert!(!demand_wants(&b, 0, 30000));
        assert!(demand_wants(&b, 4, 30000));
        assert!([&a, &b].iter().any(|d| demand_wants(d, 0, 30000)));
    }
}

#[cfg(test)]
mod automatic_tests {
    use super::*;
    #[test]
    fn transient_enqueue_rejections_do_not_memoize_automatic_verification() {
        for outcome in [
            EnqueueOutcome::QueueFull,
            EnqueueOutcome::JobCancelling {
                retry_after_ms: 1000,
            },
            EnqueueOutcome::ProducerFenced,
            EnqueueOutcome::RequestFenced,
        ] {
            assert!(!memoizes_automatic(&outcome));
        }
        assert!(memoizes_automatic(&EnqueueOutcome::Accepted {
            job_id: "job".into(),
            receipt_expires_ms: 1000
        }));
        assert!(memoizes_automatic(&EnqueueOutcome::Existing {
            job_id: "job".into(),
            receipt_expires_ms: 1000,
            cancelled: true
        }));
    }
    #[tokio::test]
    async fn queued_automatic_verification_cannot_overwrite_current_boot_explicit_failure() {
        let state = crate::http::source_actor_test_state();
        let selected = profile::selected_profile();
        let id = uuid::Uuid::new_v4().to_string();
        state
            .subtitle_models
            .translation
            .failures
            .write()
            .await
            .insert(
                selected.id().expect("profile"),
                "model_verification_failed".into(),
            );
        let now = crate::state::clock_ms();
        state.store.enqueue_job(EnqueueJob{id:id.clone(),payload:JobPayload::SubtitleModelOperation{translation_profile:Some(selected.clone()),action:plurx_core::store::background_jobs_transcription::ModelAction::TranslationVerify,model_id:selected.model_id.clone(),catalog_revision:crate::subtitle_models::catalog().revision.clone(),model_sha256:selected.model_sha256.clone(),adapter_contract:selected.adapter_contract.clone(),target_node_id:state.node_id.clone()},dedupe_key:format!("translation-auto:{}","a".repeat(64)),priority:0,not_before_ms:now,now_ms:now,request:JobRequest{scope:"translation-auto-test".into(),request_id:id.clone(),request_digest:"b".repeat(64),consumer_kind:"subtitle_model_operation".into(),consumer_ref:id.clone(),target_node_id:Some(state.node_id.clone()),deadline_ms:None,retain_identity:true}}).await.expect("queued automatic job predates failure");
        worker_pass(&state, &CancellationToken::new())
            .await
            .expect("worker pass");
        let job = state
            .store
            .background_job(&id)
            .await
            .expect("lookup")
            .expect("retained");
        assert_eq!(
            job.state,
            plurx_core::store::background_jobs::JobState::Cancelled
        );
        assert!(state
            .subtitle_models
            .translation
            .failures
            .read()
            .await
            .contains_key(&selected.id().expect("profile")));
        assert!(read(
            &state,
            TranslationQuery::Capabilities {
                node_id: state.node_id.clone()
            }
        )
        .await
        .expect("capabilities")
        .is_empty());
        state.shutdown.cancel();
    }
}
