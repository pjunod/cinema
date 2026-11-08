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

/// The background pass, driven through the same Store and fixtures.
#[cfg(test)]
mod background_pass_tests {
    use super::*;
    use crate::background_jobs::analysis_reconcile::{
        reconcile_retired_against, ReconcileContext, ReconcileReport,
    };
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use plurx_core::store::{AnalysisRequest, LiveAnalysisEngines, NewAnalysisRequest};
    use std::collections::{BTreeMap, BTreeSet};

    fn inventory(engines: &[(&str, &str)], members: Option<&[&str]>) -> LiveAnalysisEngines {
        LiveAnalysisEngines {
            engines: engines
                .iter()
                .map(|(node, engine)| ((*node).to_owned(), (*engine).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            members: members.map(|members| {
                members
                    .iter()
                    .map(|node| (*node).to_owned())
                    .collect::<BTreeSet<_>>()
            }),
        }
    }

    async fn fixture_file(state: &AppState, name: &str) -> i64 {
        fixture_file_at(
            state,
            name,
            &format!("/missing-reconcile-{name}/movie.mkv"),
            100,
            1,
        )
        .await
    }

    async fn fixture_file_at(
        state: &AppState,
        name: &str,
        path: &str,
        size: i64,
        mtime: i64,
    ) -> i64 {
        let library = state
            .store
            .create_library(&NewLibrary {
                name: format!("Reconcile {name}"),
                kind: LibraryKind::Movies,
                paths: vec![format!("/missing-reconcile-{name}").into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Reconcile {name}"),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        state
            .store
            .upsert_file(item, path, size, mtime, &ProbeResult::default())
            .await
            .expect("file")
    }

    async fn queued(state: &AppState, name: &str, engine: &str, target: &str) -> AnalysisRequest {
        let file_id = fixture_file(state, name).await;
        enqueue(state, name, file_id, (100, 1), engine, target).await
    }

    /// A queued row over a real source on this node, so the reassign path's
    /// source check passes.
    async fn queued_on_disk(
        state: &AppState,
        dir: &std::path::Path,
        name: &str,
        engine: &str,
        target: &str,
    ) -> AnalysisRequest {
        let path = dir.join(format!("{name}.mkv"));
        std::fs::write(&path, [0_u8; 100]).expect("source");
        let mtime = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs() as i64;
        let file_id = fixture_file_at(state, name, path.to_str().expect("utf-8"), 100, mtime).await;
        enqueue(state, name, file_id, (100, mtime), engine, target).await
    }

    async fn enqueue(
        state: &AppState,
        name: &str,
        file_id: i64,
        (source_size, source_mtime): (i64, i64),
        engine: &str,
        target: &str,
    ) -> AnalysisRequest {
        state
            .store
            .enqueue_analysis_request(&NewAnalysisRequest {
                request_id: format!("reconcile-{name}"),
                file_id,
                source_size,
                source_mtime,
                component: "fragment_index".into(),
                pipeline_version: engine.into(),
                video_identity: "selected-video".into(),
                requested_generation: format!("{name}-generation"),
                priority: "normal".into(),
                trigger: "background".into(),
                force_rebuild: false,
                target_node_id: target.into(),
                not_before_ms: 1,
                created_at_ms: 1,
            })
            .await
            .expect("enqueue")
    }

    /// Two digests queued, only one target fresh: exactly the rows whose
    /// target reports a different engine are replaced; a current row and a
    /// row whose target is unreported stay as they are.
    #[tokio::test]
    async fn the_background_pass_retires_only_rows_whose_own_target_reports_a_new_engine() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let (live, old) = ("a".repeat(64), "b".repeat(64));
        let local = state.node_id.clone();
        let retired = queued(&state, "retired", &old, &local).await;
        let current = queued(&state, "current", &live, &local).await;
        let unfresh = queued(&state, "unfresh", &old, "silent-peer").await;
        let engines = inventory(
            &[(local.as_str(), live.as_str())],
            Some(&[local.as_str(), "silent-peer"]),
        );
        let ctx = ReconcileContext::of_state(&state);
        let mut cursor = String::new();
        let report = reconcile_retired_against(ctx, &engines, &mut cursor, 400, false)
            .await
            .expect("pass");
        assert_eq!(
            (
                report.retired,
                report.replaced,
                report.skipped_unfresh,
                report.wrapped
            ),
            (1, 1, 1, true)
        );
        assert!(cursor.is_empty(), "a wrapped pass starts over next time");
        let after = |id: &str| {
            let state = state.clone();
            let id = id.to_owned();
            async move {
                state
                    .store
                    .analysis_request(&id)
                    .await
                    .expect("read")
                    .expect("row")
            }
        };
        let predecessor = after(&retired.request_id).await;
        assert_eq!(predecessor.state, "cancelled");
        assert_eq!(predecessor.last_error_code, "request_reconciled");
        assert_eq!(after(&current.request_id).await.state, "queued");
        assert_eq!(after(&unfresh.request_id).await.state, "queued");
        let page = state
            .store
            .analysis_reconciliation_page("", 100)
            .await
            .expect("page");
        let successor = page
            .iter()
            .find(|row| row.file_id == retired.file_id && row.state == "queued")
            .expect("a queued successor under the live engine");
        assert_eq!(successor.pipeline_version, live);
        assert_eq!(successor.target_node_id, local);

        // A second pass finds nothing left to retire; a learner's pass
        // ignores rows that name another node.
        let again = reconcile_retired_against(ctx, &engines, &mut cursor, 400, true)
            .await
            .expect("pass");
        assert_eq!((again.retired, again.replaced), (0, 0));
    }

    /// No inventory, no retirement: an empty map is unknown, not "every
    /// engine retired".
    #[tokio::test]
    async fn an_empty_inventory_retires_nothing() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let row = queued(&state, "empty", &"b".repeat(64), &state.node_id.clone()).await;
        let mut cursor = String::new();
        let report = reconcile_retired_against(
            ReconcileContext::of_state(&state),
            &LiveAnalysisEngines::default(),
            &mut cursor,
            400,
            false,
        )
        .await
        .expect("pass");
        assert_eq!(report, ReconcileReport::default());
        assert_eq!(
            state
                .store
                .analysis_request(&row.request_id)
                .await
                .expect("read")
                .expect("row")
                .state,
            "queued"
        );
    }

    /// A row some live worker can still claim is not stranded, whatever its
    /// own target runs: an idle member running the row's engine takes it
    /// through the claim's cross-node arm. The pass and the health's
    /// `incompatible` agree on that (review S7); before, the pass replaced
    /// it because its target reported something else.
    #[tokio::test]
    async fn a_row_another_live_worker_can_claim_is_left_alone() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let (mine, theirs) = ("a".repeat(64), "b".repeat(64));
        let local = state.node_id.clone();
        let row = queued(&state, "claimable", &theirs, &local).await;
        let engines = inventory(
            &[
                (local.as_str(), mine.as_str()),
                ("idle-peer", theirs.as_str()),
            ],
            Some(&[local.as_str(), "idle-peer"]),
        );
        let mut cursor = String::new();
        let report = reconcile_retired_against(
            ReconcileContext::of_state(&state),
            &engines,
            &mut cursor,
            400,
            false,
        )
        .await
        .expect("pass");
        assert_eq!((report.retired, report.replaced), (0, 0));
        let after = state
            .store
            .analysis_request(&row.request_id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(after.state, "queued");
        assert!(!engines.is_stranded(&after));
    }

    /// A stranded row whose target is not a committed member is re-homed
    /// here through the reassign path, source checked on this node; a
    /// silent member's row stays; a removed target's row is never touched —
    /// removal retires it and the reconcile statements refuse it.
    #[tokio::test]
    async fn a_stranded_row_whose_target_left_membership_is_rehomed_here() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let dir = tempfile::tempdir().expect("sources");
        let (live, old) = ("a".repeat(64), "b".repeat(64));
        let local = state.node_id.clone();
        let departed = queued_on_disk(&state, dir.path(), "departed", &old, "departed-node").await;
        let silent = queued_on_disk(&state, dir.path(), "silent", &old, "silent-member").await;
        let removed = queued_on_disk(&state, dir.path(), "removed", &old, "removed-node").await;
        state
            .store
            .put_setting("internal.cluster_job_owner_removed.removed-node", "1")
            .await
            .expect("removal fence");
        let engines = inventory(
            &[(local.as_str(), live.as_str())],
            Some(&[local.as_str(), "silent-member"]),
        );
        let mut cursor = String::new();
        let report = reconcile_retired_against(
            ReconcileContext::of_state(&state),
            &engines,
            &mut cursor,
            400,
            false,
        )
        .await
        .expect("pass");
        assert_eq!(
            (
                report.retired,
                report.absent_target,
                report.replaced,
                report.skipped_unfresh
            ),
            (2, 2, 1, 1),
            "departed and removed are absent; only departed commits; silent waits"
        );
        let state_of = |id: String| {
            let state = state.clone();
            async move {
                state
                    .store
                    .analysis_request(&id)
                    .await
                    .expect("read")
                    .expect("row")
                    .state
            }
        };
        assert_eq!(state_of(departed.request_id.clone()).await, "cancelled");
        assert_eq!(state_of(silent.request_id.clone()).await, "queued");
        assert_eq!(state_of(removed.request_id.clone()).await, "queued");
        let page = state
            .store
            .analysis_reconciliation_page("", 100)
            .await
            .expect("page");
        let successor = page
            .iter()
            .find(|row| row.file_id == departed.file_id && row.state == "queued")
            .expect("a queued successor on this node");
        assert_eq!(
            (
                successor.target_node_id.as_str(),
                successor.pipeline_version.as_str()
            ),
            (local.as_str(), live.as_str())
        );
    }
}
