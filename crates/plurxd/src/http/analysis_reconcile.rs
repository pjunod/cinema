//! Explicit, previewed reconciliation of pre-worker analysis requests.

use std::collections::BTreeSet;

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use super::{error::ApiError, extract::AdminUser};
use crate::background_jobs::analysis_reconcile::{candidate, engines};
use crate::state::AppState;

const PAGE_SIZE: i64 = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default = "preview_default")]
    dry_run: bool,
    #[serde(default)]
    cursor: String,
    #[serde(default)]
    reassign_unavailable: bool,
    #[serde(default)]
    candidates: Vec<Selection>,
}

fn preview_default() -> bool {
    true
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    request_id: String,
    candidate_id: String,
}

/// POST /analysis/reconcile. Preview uses stable keyset pages; apply acts only
/// on exact selections and re-derives every replacement from current facts.
pub async fn reconcile(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(params): Json<Params>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if params.cursor.len() > 128 || params.candidates.len() > PAGE_SIZE as usize {
        return Err(ApiError::BadRequest(
            "Reconciliation page is too large.".into(),
        ));
    }
    if params.dry_run && !params.candidates.is_empty()
        || !params.dry_run && (params.candidates.is_empty() || !params.cursor.is_empty())
    {
        return Err(ApiError::BadRequest(
            "Preview takes a cursor; apply takes exact candidates.".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for selection in &params.candidates {
        if selection.request_id.is_empty()
            || selection.request_id.len() > 128
            || !seen.insert(&selection.request_id)
            || selection.candidate_id.len() != 64
            || !selection
                .candidate_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ApiError::BadRequest(
                "Invalid reconciliation selection.".into(),
            ));
        }
    }
    let engines = engines(&state).await?;
    if params.dry_run {
        let requests = state
            .store
            .analysis_reconciliation_page(&params.cursor, PAGE_SIZE)
            .await?;
        let next_cursor = if requests.len() == PAGE_SIZE as usize {
            requests.last().map(|row| row.request_id.clone())
        } else {
            None
        };
        let mut rows = Vec::with_capacity(requests.len());
        for request in requests {
            rows.push(candidate(&state, &request, &engines, params.reassign_unavailable).await?);
        }
        return Ok(Json(
            serde_json::json!({"dry_run":true,"candidates":rows,"next_cursor":next_cursor,
            "node_id":state.node_id,"node_hostnames":super::system::node_hostnames(&state,true).await}),
        ));
    }
    if !state.jobs.analysis_queue_enabled().await {
        return Err(ApiError::Conflict(
            "Enable content analysis before applying reconciliation.".into(),
        ));
    }
    let mut results = Vec::new();
    for selection in params.candidates {
        let mut status = "changed";
        if let Some(old) = state.store.analysis_request(&selection.request_id).await? {
            let current = candidate(&state, &old, &engines, params.reassign_unavailable).await?;
            if current.candidate_id == selection.candidate_id {
                if let Some(replacement) = current.replacement {
                    if state
                        .store
                        .reconcile_analysis_request(&old, &replacement, crate::state::clock_ms())
                        .await?
                    {
                        status = "reconciled";
                    }
                }
            }
        }
        results.push(serde_json::json!({"request_id":selection.request_id,"status":status}));
    }
    super::analysis::kick_analysis_queue(&state);
    Ok(Json(serde_json::json!({"dry_run":false,"results":results})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use plurx_core::store::NewAnalysisRequest;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn reconciliation_uses_target_engine_and_fences_preview_identity() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Reconciliation".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/missing-reconciliation-fixture".into()],
                anime: false,
            })
            .await
            .expect("reconciliation fixture");
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Old analysis".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("reconciliation fixture");
        let file_id = state
            .store
            .upsert_file(
                item,
                "/missing-reconciliation-fixture/movie.mkv",
                100,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("reconciliation fixture");
        let old = state
            .store
            .enqueue_analysis_request(&NewAnalysisRequest {
                request_id: "obsolete".into(),
                file_id,
                source_size: 100,
                source_mtime: 1,
                component: "fragment_index".into(),
                pipeline_version: "obsolete-engine".into(),
                video_identity: "selected-video".into(),
                requested_generation: "old-generation".into(),
                priority: "normal".into(),
                trigger: "background".into(),
                force_rebuild: false,
                target_node_id: "peer-node".into(),
                not_before_ms: 1,
                created_at_ms: 1,
            })
            .await
            .expect("reconciliation fixture");
        let versions = BTreeMap::from([
            (state.node_id.clone(), "a".repeat(64)),
            ("peer-node".into(), "b".repeat(64)),
        ]);
        let preview = candidate(&state, &old, &versions, false)
            .await
            .expect("reconciliation fixture");
        assert!(preview.eligible);
        assert_eq!(preview.reason, "engine_changed");
        let replacement = preview
            .replacement
            .as_ref()
            .expect("reconciliation fixture");
        assert_eq!(replacement.pipeline_version, "b".repeat(64));
        assert_eq!(replacement.target_node_id, "peer-node");
        assert_eq!(replacement.video_identity, old.video_identity);
        assert!(replacement.request_id.len() <= 64);
        assert_eq!(
            candidate(&state, &old, &versions, false)
                .await
                .expect("reconciliation fixture")
                .candidate_id,
            preview.candidate_id,
            "unchanged preview is deterministic"
        );
        let mut updated = old.clone();
        updated.updated_at_ms += 1;
        assert_ne!(
            candidate(&state, &updated, &versions, false)
                .await
                .expect("reconciliation fixture")
                .candidate_id,
            preview.candidate_id,
            "a changed request invalidates its preview"
        );
        let unknown = BTreeMap::from([(state.node_id.clone(), "a".repeat(64))]);
        assert_eq!(
            candidate(&state, &old, &unknown, false)
                .await
                .expect("reconciliation fixture")
                .reason,
            "worker_unknown"
        );
        assert_eq!(
            candidate(&state, &old, &unknown, true)
                .await
                .expect("reconciliation fixture")
                .reason,
            "source_unavailable"
        );
        updated.state = "running".into();
        assert!(
            !candidate(&state, &updated, &versions, true)
                .await
                .expect("reconciliation fixture")
                .eligible
        );
        assert!(state
            .store
            .reconcile_analysis_request(&old, replacement, 10)
            .await
            .expect("reconciliation fixture"));
        let current = state
            .store
            .analysis_request(&replacement.request_id)
            .await
            .expect("reconciliation fixture")
            .expect("reconciliation fixture");
        assert_eq!(
            candidate(&state, &current, &versions, false)
                .await
                .expect("reconciliation fixture")
                .reason,
            "current"
        );
        let mut owned = current.clone();
        owned.requested_generation = "object-version-specific-generation".into();
        assert!(
            !candidate(&state, &owned, &versions, false)
                .await
                .expect("reconciliation fixture")
                .eligible,
            "playback's object-version generation is valid on the current engine"
        );
        owned.pipeline_version = "obsolete-prediction-engine".into();
        owned.requested_generation = "predict:producer-owned".into();
        assert_eq!(
            candidate(&state, &owned, &versions, true)
                .await
                .expect("reconciliation fixture")
                .reason,
            "producer_owned",
            "prediction expiry must retain ownership of its exact request"
        );
    }
}
