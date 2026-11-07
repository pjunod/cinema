//! Reconciliation of pre-worker analysis requests whose engine, source or
//! target changed. The admin page previews and applies it; the fragment-index
//! worker applies the retired-engine half of it on every pass
//! (`AppState::reconcile_retired_analysis_requests`).

use std::collections::BTreeMap;

use plurx_core::store::{AnalysisRequest, NewAnalysisRequest};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::http::error::ApiError;
use crate::http::internal_activity::PeerActivityOutcome;
use crate::state::AppState;

#[derive(Serialize)]
pub(crate) struct Candidate {
    pub(crate) request_id: String,
    pub(crate) candidate_id: String,
    pub(crate) file_id: String,
    pub(crate) title: String,
    pub(crate) target_node_id: String,
    pub(crate) replacement_node_id: String,
    pub(crate) reason: &'static str,
    pub(crate) eligible: bool,
    #[serde(skip)]
    pub(crate) replacement: Option<NewAnalysisRequest>,
}

/// Every live worker's fragment-index engine digest by node: this node's,
/// when its engine is current, and each voter peer whose activity snapshot
/// reported one inside the last 30 seconds. A node absent here retires
/// nothing: an unreported or stale worker is not evidence of a new engine.
pub(crate) async fn engines(state: &AppState) -> Result<BTreeMap<String, String>, ApiError> {
    let mut engines = BTreeMap::new();
    if crate::ffmpeg::fragment_index_engine_is_current().await {
        engines.insert(
            state.node_id.clone(),
            crate::ffmpeg::fragment_index_engine_digest().await,
        );
    }
    if state.membership.is_replicated() {
        let peers = state.peer_activity.snapshots().await.map_err(|_| {
            ApiError::Conflict("Worker inventory is unavailable; try again.".into())
        })?;
        let now = crate::state::clock_ms();
        for (id, outcome) in peers.iter() {
            if let PeerActivityOutcome::Answered(snapshot) = outcome {
                if let Some(worker) = &snapshot.workers {
                    if (-30_000..30_000).contains(&now.saturating_sub(worker.observed_at_ms)) {
                        if let Some(engine) = &worker.analysis_engine {
                            if engine.len() == 64 && engine.bytes().all(|b| b.is_ascii_hexdigit()) {
                                engines.insert(id.clone(), engine.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(engines)
}

/// Whether `old` should be replaced, and by what — the same decision for an
/// operator's previewed apply and for the background reconciler.
pub(crate) async fn candidate(
    state: &AppState,
    old: &AnalysisRequest,
    engines: &BTreeMap<String, String>,
    reassign: bool,
) -> Result<Candidate, ApiError> {
    let mut row = Candidate {
        request_id: old.request_id.clone(),
        candidate_id: String::new(),
        file_id: old.file_id.to_string(),
        title: format!("File {}", old.file_id),
        target_node_id: old.target_node_id.clone(),
        replacement_node_id: old.target_node_id.clone(),
        reason: "current",
        eligible: false,
        replacement: None,
    };
    if old.state != "queued" || !old.result_cache_key.is_empty() || old.cancel_requested {
        row.reason = "worker_owned";
        return Ok(row);
    }
    if old.component != "fragment_index" {
        row.reason = "other_component";
        return Ok(row);
    }
    if old.requested_generation.starts_with("predict:") {
        row.reason = "producer_owned";
        return Ok(row);
    }
    let Some(file) = state.store.get_file(old.file_id).await? else {
        row.reason = "source_missing";
        return Ok(row);
    };
    if let Some(item) = state.store.get_item(file.item_id).await? {
        row.title = item.title;
    }
    let engine = if let Some(engine) = engines.get(&old.target_node_id) {
        engine
    } else if reassign {
        let Some(engine) = engines.get(&state.node_id) else {
            row.reason = "worker_unknown";
            return Ok(row);
        };
        // A redirected request must have a source on its new node. The normal
        // worker still owns full attestation; this bounded check is only admission.
        if !matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                crate::fragment_index_cluster::inspect_copy_source(&file)
            )
            .await,
            Ok(Ok(_))
        ) {
            row.reason = "source_unavailable";
            return Ok(row);
        }
        row.replacement_node_id.clone_from(&state.node_id);
        engine
    } else {
        row.reason = "worker_unknown";
        return Ok(row);
    };
    // The forced-successor uniqueness key deliberately spans target nodes.
    // A same-engine rebuild cannot be replaced while its old slot is active.
    if old.force_rebuild
        && engine == &old.pipeline_version
        && file.size == old.source_size
        && file.mtime == old.source_mtime
        && row.replacement_node_id != old.target_node_id
    {
        row.reason = "rebuild_target";
        return Ok(row);
    }
    let generation = crate::state::analysis_request_generation(
        &file,
        &old.component,
        engine,
        &old.video_identity,
        false,
    );
    row.reason = if file.size != old.source_size || file.mtime != old.source_mtime {
        "source_changed"
    } else if row.replacement_node_id != old.target_node_id {
        "target_changed"
    } else if engine != &old.pipeline_version {
        "engine_changed"
    } else {
        "current"
    };
    if row.reason == "current" {
        return Ok(row);
    }
    // Stable identity makes an uncertain response safe to retry. A forced
    // predecessor keeps its explicit rebuild intent without generating another
    // forced generation on every preview.
    let identity = serde_json::to_vec(&(
        old,
        engine,
        &row.replacement_node_id,
        file.size,
        file.mtime,
        &generation,
    ))
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    row.candidate_id = hex::encode(Sha256::digest(identity));
    let id = row.candidate_id.clone();
    row.replacement = Some(NewAnalysisRequest {
        request_id: id.clone(),
        file_id: old.file_id,
        source_size: file.size,
        source_mtime: file.mtime,
        component: old.component.clone(),
        pipeline_version: engine.clone(),
        video_identity: old.video_identity.clone(),
        requested_generation: if old.force_rebuild { id } else { generation },
        priority: old.priority.clone(),
        trigger: old.trigger.clone(),
        force_rebuild: old.force_rebuild,
        target_node_id: row.replacement_node_id.clone(),
        not_before_ms: 0,
        created_at_ms: 0,
    });
    if old.force_rebuild {
        if let Some(occupied) = state
            .store
            .analysis_reconciliation_forced_slot(
                row.replacement.as_ref().expect("replacement derived above"),
            )
            .await?
        {
            if occupied.target_node_id != row.replacement_node_id {
                row.reason = "rebuild_target";
                row.replacement = None;
                return Ok(row);
            }
        }
    }
    row.eligible = true;
    Ok(row)
}
