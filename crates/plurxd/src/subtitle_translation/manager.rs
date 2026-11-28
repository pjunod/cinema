//! One bounded owned translation operation at a time on the serving worker.
use super::{artifacts, profile, runtime, AppState, StoreError, TranslationQuery, TranslationRead};
use crate::background_jobs::{ActiveBackgroundJob, JobFence};
use plurx_core::store::{
    background_jobs::{
        CandidateQuery, ClaimJob, EnqueueJob, JobKind, JobPayload, JobRequest, JobSettlement,
    },
    subtitle_translation::{RangeRecipe, SourceRevision, TranslationProfile},
    subtitle_translation_mutation::{TranslationAction, TranslationMutation},
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
#[derive(Clone, Copy)]
enum SpeechDemand {
    Viewer,
    Finish,
}

async fn interested(
    state: &AppState,
    source: &JobPayload,
    running_job: Option<&str>,
) -> Result<bool, StoreError> {
    if matches!(source, JobPayload::SubtitleTranslationRange { .. }) {
        if let Some(job_id) = running_job {
            return Ok(read(
                state,
                TranslationQuery::RangeInterest {
                    job_id: job_id.into(),
                },
            )
            .await?
            .first()
            .is_some_and(|row| row["interested"] == true));
        }
    }
    Ok(speech_demand(state, source).await?.is_some())
}

async fn speech_demand(
    state: &AppState,
    source: &JobPayload,
) -> Result<Option<SpeechDemand>, StoreError> {
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
        _ => return Ok(None),
    };
    let demands = read(
        state,
        TranslationQuery::LiveDemands {
            operation_id: op.clone(),
            operation_epoch: epoch,
        },
    )
    .await?;
    if demands
        .iter()
        .any(|d| core.is_none_or(|(index, width)| demand_wants(d, index, width)))
    {
        return Ok(Some(SpeechDemand::Viewer));
    }
    Ok(read(
        state,
        TranslationQuery::Operation {
            operation_id: op.clone(),
        },
    )
    .await?
    .first()
    .is_some_and(|row| {
        row["finish_requested"] == true
            && number(&row["operation_epoch"]) == Some(epoch)
            && !matches!(
                row["state"].as_str(),
                Some("complete" | "failed" | "stopped")
            )
    })
    .then_some(SpeechDemand::Finish))
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
            && b["preparation_ready"] == true
    }))
}

/// Existing signed node observations decide availability; no new liveness timer.
fn incumbent_unavailable(
    outcome: &crate::http::internal_activity::PeerActivityOutcome,
    node: &str,
    now: i64,
) -> bool {
    use crate::http::internal_activity::PeerActivityOutcome;
    match outcome {
        PeerActivityOutcome::Answered(snapshot) if snapshot.node_id == node => {
            snapshot.workers.as_ref().is_none_or(|worker| {
                !worker.accepting_work
                    || worker.observed_at_ms > now.saturating_add(5_000)
                    || worker.observed_at_ms < now.saturating_sub(30_000)
            })
        }
        _ => true,
    }
}

/// A fresh accepted Create/Demand may recover only its own operation on this
/// qualified receiver. Root Stop epochs, viewer demands and cached bytes stay intact.
pub(crate) async fn reconcile_producer(
    state: &AppState,
    operation_id: &str,
    user_id: i64,
) -> Result<(), StoreError> {
    let Some(op) = read(
        state,
        TranslationQuery::Operation {
            operation_id: operation_id.into(),
        },
    )
    .await?
    .pop() else {
        return Ok(());
    };
    if op["cached_complete"] == true
        || matches!(
            op["state"].as_str(),
            Some("stopped" | "failed" | "complete")
        )
    {
        return Ok(());
    }
    let Some(epoch) = number(&op["operation_epoch"]) else {
        return Ok(());
    };
    let Some(view) = number(&op["view_revision"]) else {
        return Ok(());
    };
    let Some(target) = op["target_node_id"].as_str() else {
        return Ok(());
    };
    let Some(preparation) = op["preparation_job_id"].as_str() else {
        return Ok(());
    };
    if target != state.node_id {
        let Ok(outcome) = state.peer_activity.owner_snapshot(target).await else {
            return Ok(());
        };
        if !incumbent_unavailable(&outcome, target, crate::state::clock_ms()) {
            return Ok(());
        }
    } else if op["preparation_state"] == "queued"
        || op["preparation_state"] == "running"
            && op["preparation_owner_boot_id"] == state.subtitle_models.boot_id.as_str()
    {
        return Ok(());
    }
    let Some(file_id) = number(&op["file_id"]) else {
        return Ok(());
    };
    let Some(file) = state.store.get_file(file_id).await? else {
        return Ok(());
    };
    if op["source_size"].as_str() != Some(file.size.to_string().as_str())
        || op["source_mtime"].as_str() != Some(file.mtime.to_string().as_str())
    {
        return Ok(());
    }
    let profile: TranslationProfile = serde_json::from_value(op["profile"].clone())
        .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?;
    if target == state.node_id
        && state
            .subtitle_models
            .translation
            .cached(&file)
            .await
            .is_some()
    {
        if let Some(asset) = op["asset_id"].as_str() {
            if bound_this_boot(state, asset, operation_id, epoch).await? {
                return Ok(());
            }
        }
    }
    if super::eligible_worker(state, &file, &profile)
        .await
        .is_err()
    {
        return Ok(());
    }
    let now = crate::state::clock_ms();
    let action = TranslationAction::Retarget {
        operation_id: operation_id.into(),
        expected_operation_epoch: epoch,
        expected_view_revision: view,
        expected_preparation_job_id: preparation.into(),
        expected_target_node_id: target.into(),
        target_node_id: state.node_id.clone(),
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_digest = plurx_core::store::subtitle_translation::digest(
        &serde_json::to_value(&action)
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?,
    )?;
    let result = state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id,
            request_id,
            request_digest,
            now_ms: now,
            intent_expires_at_ms: now + 60_000,
            action,
        })
        .await?;
    if result["outcome"] == "accepted" {
        state.subtitle_models.translation.wake.notify_one();
    }
    Ok(())
}

async fn queue_range(
    state: &AppState,
    recipe: RangeRecipe,
    producer_preparation_job_id: String,
    range_revision: i64,
    priority: u8,
) -> Result<(), StoreError> {
    recipe.validate()?;
    let now = crate::state::clock_ms();
    let id = uuid::Uuid::new_v4().to_string();
    let digest = plurx_core::store::subtitle_translation::digest(
        &serde_json::to_value(&recipe)
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?,
    )?;
    let dedupe_key = format!(
        "translation-range:{}:{}:{}:{range_revision}",
        recipe.operation_id, recipe.operation_epoch, recipe.core_index
    );
    state
        .store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::SubtitleTranslationRange {
                file_id: recipe.source.file_id,
                recipe,
                producer_preparation_job_id,
                target_node_id: state.node_id.clone(),
            },
            dedupe_key,
            priority,
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
    Ok(())
}

/// Enqueue one missing core, serving any playhead window before finishing a
/// track in the background.
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

    let mut finish = None;
    for op in ops {
        let operation_id = op["operation_id"]
            .as_str()
            .ok_or_else(|| StoreError::Task("translation operation identity".into()))?
            .to_owned();
        let epoch = number(&op["operation_epoch"])
            .ok_or_else(|| StoreError::Task("translation epoch".into()))?;
        let profile: TranslationProfile = serde_json::from_value(op["profile"].clone())
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?;
        if !super::profile::registered(&profile) {
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
        let binding = op["asset_id"]
            .as_str()
            .zip(state.subtitle_models.translation.cached(&file).await);
        let (asset, held) = match binding {
            Some((asset, held)) if bound_this_boot(state, asset, &operation_id, epoch).await? => {
                (asset, held)
            }
            _ => {
                if let Some(requested_by) = number(&op["finish_requested_by"]) {
                    // A restarted producer re-attests this operation through
                    // the same fenced source preparation as a viewer retry.
                    reconcile_producer(state, &operation_id, requested_by).await?;
                }
                continue;
            }
        };
        let duration = number(&op["duration_ms"]).unwrap_or(0);
        let make_recipe = |core_index| {
            Ok::<_, StoreError>(RangeRecipe {
                asset_id: asset.into(),
                operation_id: operation_id.clone(),
                operation_epoch: epoch,
                source: source.clone(),
                source_sha256: held.sha256().into(),
                profile: profile.clone(),
                audio_index: number(&op["audio_index"])
                    .ok_or_else(|| StoreError::Task("translation audio identity".into()))?,
                spoken_language: op["spoken_language"].as_str().unwrap_or_default().into(),
                duration_ms: duration,
                core_index,
            })
        };
        let preparation_job = op["preparation_job_id"]
            .as_str()
            .ok_or_else(|| StoreError::Task("translation producer missing".into()))?;
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
                queue_range(
                    state,
                    make_recipe(index)?,
                    preparation_job.into(),
                    prior
                        .first()
                        .and_then(|r| number(&r["range_revision"]))
                        .unwrap_or(0),
                    1,
                )
                .await?;
                return Ok(true);
            }
        }
        if finish.is_none() {
            let rows = read(
                state,
                TranslationQuery::FinishCore {
                    operation_id: operation_id.clone(),
                    operation_epoch: epoch,
                },
            )
            .await?;
            if let Some(row) = rows.first() {
                let index = number(&row["core_index"])
                    .ok_or_else(|| StoreError::Task("translation core identity".into()))?;
                finish = Some((
                    make_recipe(index)?,
                    preparation_job.to_owned(),
                    number(&row["range_revision"]).unwrap_or(0),
                ));
            }
        }
    }
    if let Some((recipe, preparation_job, revision)) = finish {
        queue_range(state, recipe, preparation_job, revision, 0).await?;
    }
    Ok(demanded)
}

async fn execute(
    state: &AppState,
    source: &JobPayload,
    fence: &JobFence,
    cancel: &CancellationToken,
    admission: &crate::transcode::FragmentAdmission,
) -> Result<bool, StoreError> {
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
    if !profile::registered(selected) {
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
    let audio_index = match source {
        JobPayload::SubtitleTranslationPrepare { audio_index, .. } => *audio_index,
        JobPayload::SubtitleTranslationRange { recipe, .. } => recipe.audio_index,
        _ => unreachable!(),
    };
    super::selected_file_capability(state, &file, audio_index)
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
    if !interested(
        state,
        source,
        fence.snapshot().await.as_ref().map(|t| t.job_id.as_str()),
    )
    .await?
        || cancel.is_cancelled()
    {
        return Err(StoreError::Task("demand_expired".into()));
    }
    match source {
        JobPayload::SubtitleTranslationPrepare {
            spoken_language, ..
        } => {
            let resolved_language = if spoken_language == "auto" {
                let spec = profile::model_spec_for(selected)?;
                let detection = runtime::detect_spoken_language(
                    state,
                    &file,
                    &held,
                    audio_index,
                    &spec,
                    fence,
                    cancel,
                );
                tokio::pin!(detection);
                Some(tokio::select! {
                    result = &mut detection => result?,
                    () = crate::subtitle_transcription::watch_speech_capacity(&state.transcode, admission) => {
                        cancel.cancel();
                        let _ = detection.await;
                        return Err(StoreError::Task("waiting_for_playback_capacity".into()));
                    }
                })
            } else {
                None
            };
            fence
                .bind_translation_source(
                    source.clone(),
                    held.sha256().into(),
                    artifacts::identity_key(held.identity()),
                    resolved_language,
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
            let spec = profile::model_spec_for(selected)?;
            let hash =
                artifacts::open_model_cancellable(&state.subtitle_models.root, &spec, cancel);
            tokio::pin!(hash);
            let model = tokio::select! {result=&mut hash=>result.map_err(StoreError::Task)?,()=crate::subtitle_transcription::watch_speech_capacity(&state.transcode,admission)=>{cancel.cancel();let _=hash.await;return Err(StoreError::Task("waiting_for_playback_capacity".into()));}};
            // The current receipt must match the exact model identity opened
            // AFTER admission; metadata observed before a claim is insufficient.
            if !super::workers_public(state)
                .await
                .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
                .iter()
                .any(|w| {
                    w["capabilities"].as_array().is_some_and(|caps| {
                        caps.iter().any(|c| {
                            c["status"] == "verified"
                                && c["profile_id"].as_str() == selected.id().ok().as_deref()
                        })
                    })
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
            // Model operations, including queued legacy translation actions,
            // belong to `subtitle_models::worker_pass`.
            kinds: vec![
                JobKind::SubtitleTranslationPrepare,
                JobKind::SubtitleTranslationRange,
            ],
            after,
            now_ms: crate::state::clock_ms(),
            limit: 32,
            admissible_only: false,
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
        if let JobPayload::SubtitleTranslationRange {
            recipe,
            producer_preparation_job_id,
            target_node_id,
            ..
        } = &source
        {
            let current = read(
                state,
                TranslationQuery::Operation {
                    operation_id: recipe.operation_id.clone(),
                },
            )
            .await?;
            if producer_preparation_job_id.is_empty()
                || current.first().is_none_or(|op| {
                    op["preparation_job_id"] != producer_preparation_job_id.as_str()
                        || op["target_node_id"] != target_node_id.as_str()
                        || op["cached_complete"] == true
                        || number(&op["operation_epoch"]) != Some(recipe.operation_epoch)
                })
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
        }
        let selected = match &source {
            JobPayload::SubtitleTranslationPrepare { profile, .. } => profile.clone(),
            JobPayload::SubtitleTranslationRange { recipe, .. } => recipe.profile.clone(),
            _ => continue,
        };
        if shutdown.is_cancelled() || !authority.may_execute_job(source.kind()).await {
            continue;
        }
        let Some(demand) = speech_demand(state, &source).await? else {
            state
                .store
                .cancel_job(plurx_core::store::background_jobs::CancelJob {
                    job_id: candidate.id.clone(),
                    now_ms: crate::state::clock_ms(),
                })
                .await?;
            continue;
        };
        if !super::workers_public(state)
            .await
            .map_err(|_| StoreError::Task("worker_observation_unavailable".into()))?
            .iter()
            .any(|w| {
                w["capabilities"].as_array().is_some_and(|c| {
                    c.iter().any(|v| {
                        v["status"] == "verified"
                            && v["profile_id"].as_str() == selected.id().ok().as_deref()
                    })
                })
            })
        {
            continue;
        }
        let admission = match demand {
            SpeechDemand::Viewer => state.transcode.admit_progressive_speech(shutdown).await,
            SpeechDemand::Finish => state.transcode.admit_speech().await,
        };
        let Some(admission) = admission.filter(|a| state.transcode.speech_capacity_available(a))
        else {
            continue;
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
        )
        .await?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let operation = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            execute(state, &source, &fence, &cancel, &admission),
        );
        let operation = crate::background_jobs::budgeted(fence.source_budget(), operation);
        tokio::pin!(operation);
        let result = loop {
            tokio::select! {
                result=&mut operation=>break result,
                ()=shutdown.cancelled()=>{cancel.cancel();break operation.await;},
                ()=tokio::time::sleep(Duration::from_millis(250))=>{
                    match tokio::time::timeout(Duration::from_secs(2),interested(state,&source,Some(&job.id))).await{Ok(Ok(true))=>{},Ok(Ok(false))|Ok(Err(_))|Err(_)=>{cancel.cancel();let _=operation.await;break Err(StoreError::Task("demand_expired".into()));}}
                }
            }
        };
        match result {
            Ok(true) => {}
            Ok(false) => {
                let _ = fence
                    .settle(JobSettlement::Stop {
                        error_code: "translation_publication_refused".into(),
                    })
                    .await;
            }
            Err(error) => {
                let message = error.to_string();
                if message.contains("demand_expired") {
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
        "spoken_language_unknown",
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
        "model_self_test_failed",
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
    fn failed_model_speech_test_retains_trusted_setup_recovery_reason() {
        assert_eq!(
            safe_reason("task failed: model_self_test_failed"),
            "model_self_test_failed"
        );
        assert_eq!(
            safe_reason("untrusted raw recognizer output"),
            "translation_failed"
        );
    }
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
mod receipt_tests {
    use super::*;
    use plurx_core::{
        fs_secure::FileIdentity, store::background_jobs_transcription::SubtitleModelReceipt,
    };

    /// Progressive translation and transcription accept the one model
    /// receipt: a node with only a current-boot receipt for `small` (no
    /// per-profile capability row exists any more) is verified for both
    /// speech tasks, and the same receipt is refused after a reboot.
    #[test]
    fn progressive_profiles_accept_the_model_receipt_and_refuse_it_after_reboot() {
        let spec = profile::model_spec();
        let model = FileIdentity {
            device: 7,
            inode: 11,
            size: spec.download_bytes,
            changed_seconds: 1,
            changed_nanoseconds: 2,
        };
        let engine_file = FileIdentity { inode: 12, ..model };
        let engine = crate::subtitle_models::EngineIdentity {
            adapter_contract: &crate::subtitle_models::catalog().adapter_contract,
            binary_sha256: &"e".repeat(64),
            identity: engine_file,
        };
        let receipt = SubtitleModelReceipt {
            node_id: "worker".into(),
            boot_id: "boot-one".into(),
            model_id: spec.id.clone(),
            model_sha256: spec.sha256.clone(),
            adapter_contract: engine.adapter_contract.to_owned(),
            engine_sha256: engine.binary_sha256.to_owned(),
            model_file_identity: artifacts::identity_key(model),
            engine_file_identity: artifacts::identity_key(engine_file),
            job_id: "job".into(),
            fence: 1,
            verified_at_ms: 1234,
        };
        let receipts = [receipt];
        for selected in profile::profiles() {
            let status = |boot: &str| {
                super::super::profile_capability(
                    &selected,
                    &spec,
                    Ok(engine),
                    &Ok(model),
                    &receipts,
                    false,
                    super::super::WorkerBoot {
                        node_id: "worker",
                        boot_id: boot,
                    },
                )
                .expect("capability")
            };
            let current = status("boot-one");
            assert_eq!(current["status"], "verified", "{:?}", selected.task);
            assert_eq!(current["verified_at_ms"], 1234);
            let rebooted = status("boot-two");
            assert_eq!(rebooted["status"], "unverified_cache");
            assert_eq!(rebooted["reason"], "translation_unverified");
            let replaced = super::super::profile_capability(
                &selected,
                &spec,
                Ok(engine),
                &Ok(FileIdentity { inode: 99, ..model }),
                &receipts,
                false,
                super::super::WorkerBoot {
                    node_id: "worker",
                    boot_id: "boot-one",
                },
            )
            .expect("capability");
            assert_eq!(replaced["status"], "unverified_cache");
        }
    }
}
