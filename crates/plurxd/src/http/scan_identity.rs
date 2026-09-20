//! Admin-only preview, status and apply surface for bounded show repair.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::store::{
    plan_identity_repair, IdentityRepairOutcome, IdentityRepairPlan, IdentityRepairSnapshot,
    IDENTITY_REPAIR_PLAN_BYTES_MAX, IDENTITY_REPAIR_SHOWS_MAX, IDENTITY_REPAIR_SHOWS_MIN,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

const PLAN_TTL: Duration = Duration::from_secs(15 * 60);
const PLAN_CACHE_MAX: usize = 8;

#[derive(Clone, Default)]
pub(crate) struct IdentityRepairCache {
    inner: Arc<Mutex<HashMap<String, CachedPlan>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CachedStatus {
    Ready,
    Applying,
    Applied,
    OutcomeUnknown,
    Stale,
    Blocked,
}

impl CachedStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Stale => "stale",
            Self::Blocked => "blocked",
        }
    }

    fn completed(self) -> bool {
        matches!(self, Self::Applied | Self::Stale | Self::Blocked)
    }
}

#[derive(Clone)]
struct CachedPlan {
    plan_id: String,
    node_id: String,
    admin_user_id: i64,
    library_id: i64,
    created_at: tokio::time::Instant,
    expires_at_unix: i64,
    snapshot: IdentityRepairSnapshot,
    plan: IdentityRepairPlan,
    status: CachedStatus,
}

#[derive(Deserialize)]
pub(crate) struct PreviewRequest {
    show_ids: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct ApplyRequest {
    fingerprint: String,
    #[serde(default)]
    accept_watch_conflicts: bool,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn parse_show_ids(values: Vec<String>) -> Result<Vec<i64>, ApiError> {
    if !(IDENTITY_REPAIR_SHOWS_MIN..=IDENTITY_REPAIR_SHOWS_MAX).contains(&values.len()) {
        return Err(ApiError::BadRequest(format!(
            "show_ids must contain {IDENTITY_REPAIR_SHOWS_MIN}..={IDENTITY_REPAIR_SHOWS_MAX} IDs"
        )));
    }
    let mut ids = values
        .into_iter()
        .map(|value| {
            if value.is_empty()
                || value.starts_with('+')
                || (value.len() > 1 && value.starts_with('0'))
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(ApiError::BadRequest(
                    "show_ids must be canonical positive decimal strings".to_owned(),
                ));
            }
            value
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| {
                    ApiError::BadRequest(
                        "show_ids must be canonical positive decimal strings".to_owned(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ApiError::BadRequest("show_ids must be unique".to_owned()));
    }
    Ok(ids)
}

fn store_error(error: plurx_core::error::StoreError) -> ApiError {
    let message = error.to_string();
    if message.contains("repair_too_large") {
        ApiError::typed(
            StatusCode::PAYLOAD_TOO_LARGE,
            "repair_too_large",
            "the repair exceeds a row, transaction, or 2 MiB preview bound",
        )
    } else if message.contains("library not found") {
        ApiError::NotFound("library")
    } else {
        ApiError::from(error)
    }
}

impl IdentityRepairCache {
    async fn insert(
        &self,
        node_id: &str,
        admin_user_id: i64,
        library_id: i64,
        snapshot: IdentityRepairSnapshot,
        plan: IdentityRepairPlan,
    ) -> Result<CachedPlan, ApiError> {
        let encoded_bytes = serde_json::to_vec(&(&snapshot, &plan))
            .map_err(|error| ApiError::Internal(error.to_string()))?
            .len();
        if encoded_bytes > IDENTITY_REPAIR_PLAN_BYTES_MAX {
            return Err(ApiError::typed(
                StatusCode::PAYLOAD_TOO_LARGE,
                "repair_too_large",
                "the repair preview exceeds 2 MiB",
            ));
        }
        let now = tokio::time::Instant::now();
        let mut plans = self.inner.lock().await;
        plans.retain(|_, entry| {
            entry.created_at + PLAN_TTL > now || entry.status == CachedStatus::Applying
        });
        if plans.len() >= PLAN_CACHE_MAX {
            let candidate = plans
                .iter()
                .filter(|(_, entry)| entry.status.completed())
                .min_by_key(|(_, entry)| entry.created_at)
                .or_else(|| {
                    plans
                        .iter()
                        .filter(|(_, entry)| entry.status != CachedStatus::Applying)
                        .min_by_key(|(_, entry)| entry.created_at)
                })
                .map(|(id, _)| id.clone());
            let Some(candidate) = candidate else {
                return Err(ApiError::typed(
                    StatusCode::CONFLICT,
                    "repair_busy",
                    "all identity repair preview slots are applying",
                ));
            };
            plans.remove(&candidate);
        }
        let entry = CachedPlan {
            plan_id: uuid::Uuid::new_v4().to_string(),
            node_id: node_id.to_owned(),
            admin_user_id,
            library_id,
            created_at: now,
            expires_at_unix: now_unix() + PLAN_TTL.as_secs() as i64,
            status: if plan.ready() {
                CachedStatus::Ready
            } else {
                CachedStatus::Blocked
            },
            snapshot,
            plan,
        };
        plans.insert(entry.plan_id.clone(), entry.clone());
        Ok(entry)
    }

    async fn get(
        &self,
        plan_id: &str,
        admin_user_id: i64,
        library_id: i64,
        node_id: &str,
    ) -> Result<CachedPlan, ApiError> {
        let mut plans = self.inner.lock().await;
        let Some(entry) = plans.get(plan_id).cloned() else {
            return Err(ApiError::typed(
                StatusCode::GONE,
                "repair_plan_expired",
                "the preview is unknown on this node; create a fresh preview",
            ));
        };
        if entry.admin_user_id != admin_user_id || entry.library_id != library_id {
            return Err(ApiError::NotFound("identity repair plan"));
        }
        if entry.node_id != node_id {
            return Err(ApiError::typed_detail(
                StatusCode::CONFLICT,
                "repair_wrong_node",
                "the preview must be used on its origin node",
                json!({ "node_id": entry.node_id }),
            ));
        }
        if entry.created_at + PLAN_TTL <= tokio::time::Instant::now()
            && entry.status != CachedStatus::Applying
        {
            plans.remove(plan_id);
            return Err(ApiError::typed(
                StatusCode::GONE,
                "repair_plan_expired",
                "the preview expired; create a fresh preview of current rows",
            ));
        }
        Ok(entry)
    }

    async fn set_status(&self, plan_id: &str, status: CachedStatus) {
        if let Some(entry) = self.inner.lock().await.get_mut(plan_id) {
            entry.status = status;
        }
    }
}

fn state_json(entry: &CachedPlan) -> Value {
    let plan = &entry.plan;
    let watch = |state: &plurx_core::store::IdentityRepairWatch| {
        json!({
            "user_id": state.user_id.to_string(),
            "item_id": state.item_id.to_string(),
            "position_ms": state.position_ms,
            "duration_ms": state.duration_ms,
            "watched": state.watched,
            "updated_at": state.updated_at,
        })
    };
    json!({
        "plan_id": entry.plan_id,
        "node_id": entry.node_id,
        "library_id": entry.library_id.to_string(),
        "expires_at": entry.expires_at_unix,
        "fingerprint": plan.fingerprint,
        "status": entry.status.as_str(),
        "source_directory": plan.source_directory,
        "survivor_show_id": plan.survivor_show_id.map(|id| id.to_string()),
        "input_show_ids": plan.input_show_ids.iter().map(i64::to_string).collect::<Vec<_>>(),
        "item_moves": plan.item_moves.iter().map(|movement| json!({
            "item_id": movement.item_id.to_string(),
            "expected_parent_id": movement.expected_parent_id.to_string(),
            "new_parent_id": movement.new_parent_id.to_string(),
        })).collect::<Vec<_>>(),
        "file_moves": plan.file_moves.iter().map(|movement| json!({
            "file_id": movement.file_id.to_string(),
            "expected_item_id": movement.expected_item_id.to_string(),
            "new_item_id": movement.new_item_id.to_string(),
        })).collect::<Vec<_>>(),
        "retired_item_ids": plan.retired_item_ids.iter().map(i64::to_string).collect::<Vec<_>>(),
        "watch_copies": plan.watch_copies.iter().map(|copy| json!({
            "user_id": copy.user_id.to_string(),
            "source_item_id": copy.source_item_id.to_string(),
            "destination_item_id": copy.destination_item_id.to_string(),
            "state": watch(&copy.state),
        })).collect::<Vec<_>>(),
        "watch_conflicts": plan.watch_conflicts.iter().map(|conflict| json!({
            "user_id": conflict.user_id.to_string(),
            "source_item_id": conflict.source_item_id.to_string(),
            "destination_item_id": conflict.destination_item_id.to_string(),
            "source": watch(&conflict.source),
            "destination": watch(&conflict.destination),
            "resolution": conflict.resolution,
        })).collect::<Vec<_>>(),
        "reference_summary": plan.reference_summary,
        "blockers": plan.blockers,
        "before_counts": plan.before_counts,
        "expected_after_counts": plan.expected_after_counts,
        "file_availability": plan.file_availability,
    })
}

pub(crate) async fn preview(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(library_id): Path<i64>,
    Json(body): Json<PreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    if library_id <= 0 {
        return Err(ApiError::BadRequest(
            "library ID must be positive".to_owned(),
        ));
    }
    let show_ids = parse_show_ids(body.show_ids)?;
    let snapshot = state
        .store
        .identity_repair_snapshot(library_id, &show_ids)
        .await
        .map_err(store_error)?;
    let plan = plan_identity_repair(snapshot.clone()).map_err(store_error)?;
    let entry = state
        .jobs
        .identity_repairs
        .insert(&state.node_id, admin.id, library_id, snapshot, plan)
        .await?;
    Ok(Json(state_json(&entry)))
}

pub(crate) async fn status(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path((library_id, plan_id)): Path<(i64, String)>,
) -> Result<Json<Value>, ApiError> {
    let entry = state
        .jobs
        .identity_repairs
        .get(&plan_id, admin.id, library_id, &state.node_id)
        .await?;
    Ok(Json(state_json(&entry)))
}

pub(crate) async fn apply(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path((library_id, plan_id)): Path<(i64, String)>,
    Json(body): Json<ApplyRequest>,
) -> Result<Json<Value>, ApiError> {
    if body.fingerprint.len() != 64
        || !body
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::BadRequest(
            "fingerprint must be a 64-character hexadecimal SHA-256".to_owned(),
        ));
    }
    let entry = state
        .jobs
        .identity_repairs
        .get(&plan_id, admin.id, library_id, &state.node_id)
        .await?;
    if entry.status == CachedStatus::Applied {
        return Ok(Json(state_json(&entry)));
    }
    if entry.status == CachedStatus::Applying {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "repair_busy",
            "this identity repair is already applying",
        ));
    }
    if entry.status == CachedStatus::Blocked || !entry.plan.ready() {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "repair_blocked",
            "the preview contains blockers and has no applicable plan",
        ));
    }
    if entry.status == CachedStatus::Stale {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "repair_stale",
            "catalogue state changed after preview; create a fresh preview",
        ));
    }
    if entry.status == CachedStatus::OutcomeUnknown {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "repair_outcome_unknown",
            "read this plan status on its origin node before attempting a new preview",
        ));
    }
    if body.fingerprint != entry.plan.fingerprint {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "repair_stale",
            "the supplied fingerprint does not select this cached preview",
        ));
    }
    if !entry.plan.watch_conflicts.is_empty() && !body.accept_watch_conflicts {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "watch_conflicts_unaccepted",
            "accept_watch_conflicts must acknowledge this fingerprinted conflict list",
        ));
    }
    state
        .jobs
        .identity_repairs
        .set_status(&plan_id, CachedStatus::Applying)
        .await;
    let outcome = state
        .jobs
        .apply_identity_repair(&entry.snapshot, &entry.plan)
        .await;
    match outcome {
        Ok(Some(IdentityRepairOutcome::Applied | IdentityRepairOutcome::AlreadyApplied)) => {
            state
                .jobs
                .identity_repairs
                .set_status(&plan_id, CachedStatus::Applied)
                .await;
        }
        Ok(Some(IdentityRepairOutcome::Stale)) => {
            state
                .jobs
                .identity_repairs
                .set_status(&plan_id, CachedStatus::Stale)
                .await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "repair_stale",
                "catalogue state changed after preview; create a fresh preview",
            ));
        }
        Ok(None) => {
            state
                .jobs
                .identity_repairs
                .set_status(&plan_id, CachedStatus::Ready)
                .await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "repair_busy",
                "the library scan lease is held by other work",
            ));
        }
        Err(error) => {
            state
                .jobs
                .identity_repairs
                .set_status(&plan_id, CachedStatus::OutcomeUnknown)
                .await;
            tracing::error!(library_id, plan_id, error = %error, "identity repair outcome is unknown");
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "repair_outcome_unknown",
                "the apply result is uncertain; read this plan status on the origin node",
            ));
        }
    }
    let applied = state
        .jobs
        .identity_repairs
        .get(&plan_id, admin.id, library_id, &state.node_id)
        .await?;
    Ok(Json(state_json(&applied)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_identity_repair_ids_are_strict_positive_decimal_strings() {
        assert_eq!(
            parse_show_ids(vec!["9007199254740993".to_owned(), "2".to_owned()])
                .expect("large exact IDs"),
            vec![2, 9_007_199_254_740_993]
        );
        for values in [
            vec!["1".to_owned(), "1".to_owned()],
            vec!["01".to_owned(), "2".to_owned()],
            vec!["-1".to_owned(), "2".to_owned()],
        ] {
            assert!(parse_show_ids(values).is_err());
        }
    }

    #[tokio::test]
    async fn scan_identity_plan_cache_is_admin_node_and_expiry_bound() {
        let cache = IdentityRepairCache::default();
        let snapshot = IdentityRepairSnapshot {
            library_id: 9,
            library_kind: "shows".to_owned(),
            library_paths: r#"["/media"]"#.to_owned(),
            input_show_ids: vec![1, 2],
            items: Vec::new(),
            files: Vec::new(),
            watches: Vec::new(),
            directory_owner_ids: Vec::new(),
            dependency_rows: Vec::new(),
            blockers: Vec::new(),
        };
        let plan = plan_identity_repair(snapshot.clone()).expect("bounded blocked plan");
        let entry = cache
            .insert("node-a", 7, 9, snapshot, plan)
            .await
            .expect("cache plan");

        assert!(matches!(
            cache.get(&entry.plan_id, 8, 9, "node-b").await,
            Err(ApiError::NotFound("identity repair plan"))
        ));
        assert!(matches!(
            cache.get(&entry.plan_id, 7, 9, "node-b").await,
            Err(ApiError::TypedDetail {
                code: "repair_wrong_node",
                ..
            })
        ));

        cache
            .inner
            .lock()
            .await
            .get_mut(&entry.plan_id)
            .expect("cached entry")
            .created_at = tokio::time::Instant::now() - PLAN_TTL;
        assert!(matches!(
            cache.get(&entry.plan_id, 7, 9, "node-a").await,
            Err(ApiError::Typed {
                code: "repair_plan_expired",
                ..
            })
        ));
    }
}
