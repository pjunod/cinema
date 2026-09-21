//! Background metadata classification and optional local semantic search.
pub(crate) mod semantic;
use crate::{
    http::{
        error::ApiError,
        extract::{AdminUser, AuthUser},
    },
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use plurx_core::{
    cluster::coordination::LeaseClaim,
    metadata::{
        classification::{self, Overrides},
        TmdbClient,
    },
    store::{
        classification::{Entry, Record},
        keys,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{OnceLock, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
pub const SEMANTIC_KEY: &str = "search.semantic.enabled";
#[derive(Default, Clone, Serialize)]
pub struct Status {
    pub processed: usize,
    pub labelled: usize,
    pub provider_error: Option<String>,
    pub scan_complete: bool,
}
static STATUS: OnceLock<RwLock<Status>> = OnceLock::new();
fn observation() -> &'static RwLock<Status> {
    STATUS.get_or_init(Default::default)
}
fn now() -> i64 {
    crate::media_sessions::unix_ms() / 1000
}
pub async fn worker(state: AppState, shutdown: CancellationToken) {
    let owner = uuid::Uuid::new_v4().to_string();
    let mut cursor = 0;
    loop {
        let result = async {
            if !state.jobs.may_run_cluster_jobs().await {
                return Ok::<_, ApiError>(());
            }
            let time = now() * 1000;
            let LeaseClaim::Acquired(lease) = state
                .store
                .acquire_lease("metadata-classification", &owner, time, time + 120_000)
                .await?
            else {
                return Ok(());
            };
            let result = classify_page(&state, &mut cursor).await;
            // Best-effort: the lease has a bounded expiry and a failed early
            // release delays, but cannot lose, the next classification pass.
            crate::store_result::observe(
                crate::store_result::Operation::ReleaseClassificationLease,
                crate::store_result::Discard::BestEffort,
                state.store.release_lease(&lease, now() * 1000).await,
            );
            result
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error=?e,"Metadata classification pass failed");
        }
        tokio::select! {_=shutdown.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(if cursor==0{30}else{1}))=>{}}
    }
}
pub(crate) async fn classify_page(state: &AppState, cursor: &mut i64) -> Result<(), ApiError> {
    let entries = state.store.classification_page(*cursor, 32).await?;
    if entries.is_empty() {
        *cursor = 0;
        if let Ok(mut o) = observation().write() {
            o.scan_complete = true;
        }
        return Ok(());
    }
    if *cursor == 0 {
        if let Ok(mut o) = observation().write() {
            *o = Status::default();
        }
    }
    let key = state
        .store
        .get_setting(keys::TMDB_API_KEY)
        .await?
        .filter(|v| !v.is_empty());
    let provider = key.map(TmdbClient::new);
    let mut requests = 0;
    for entry in entries {
        let input = entry.input()?;
        *cursor = input.id;
        let old = entry.record.as_ref();
        let mut keywords = old
            .map(|r| r.classification.keywords.clone())
            .unwrap_or_default();
        let identity_changed = old
            .and_then(|r| {
                serde_json::from_str::<plurx_core::store::classification::Input>(&r.source_json)
                    .ok()
            })
            .is_some_and(|previous| {
                previous.tmdb_id != input.tmdb_id || previous.kind != input.kind
            });
        if identity_changed {
            keywords.clear();
        }
        let mut checked = old
            .map(|r| r.classification.provider_checked_at)
            .unwrap_or(0);
        let mut error = old.and_then(|r| r.classification.provider_error.clone());
        if identity_changed {
            checked = 0;
            error = None;
        }
        let due = now() - checked > if error.is_some() { 3600 } else { 30 * 86400 };
        if requests < 4 && due && matches!(input.kind.as_str(), "movie" | "show") {
            if let (Some(provider), Some(id)) = (&provider, input.tmdb_id) {
                requests += 1;
                checked = now();
                match tokio::time::timeout(Duration::from_secs(15), provider.keywords(id, &input.kind)).await {
                    Ok(Ok(values)) => { keywords=values; error=None; }
                    Ok(Err(_)) => error=Some("Metadata keyword provider could not be reached; stored labels remain available.".into()),
                    Err(_) => error=Some("Metadata keyword request timed out; stored labels remain available.".into()),
                }
            }
        }
        let unchanged = entry.indexed
            && old.is_some_and(|r| {
                r.source_json == entry.source_json
                    && r.classification.version == classification::VERSION
                    && r.classification.provider_checked_at == checked
            });
        let mut result = classification::classify(&input.metadata(), keywords);
        let labelled = !result
            .terms(&old.map(|r| r.overrides.clone()).unwrap_or_default())
            .is_empty();
        if !unchanged {
            result.provider_checked_at = checked;
            result.provider_error = error.clone();
            let record = Record {
                source_json: entry.source_json,
                classification: result,
                overrides: old.map(|r| r.overrides.clone()).unwrap_or_default(),
                revision: old.map(|r| r.revision).unwrap_or(0),
            };
            if state.jobs.may_run_cluster_jobs().await {
                state.store.write_classification(input.id, &record).await?;
            }
        }
        if let Ok(mut o) = observation().write() {
            o.processed += 1;
            if error.is_some() {
                o.provider_error = error;
            }
            if labelled {
                o.labelled += 1;
            }
        }
    }
    Ok(())
}
async fn entry(state: &AppState, id: i64) -> Result<Entry, ApiError> {
    state
        .store
        .classification_page(id.saturating_sub(1), 1)
        .await?
        .into_iter()
        .find(|e| e.input().is_ok_and(|i| i.id == id))
        .ok_or(ApiError::NotFound("media item"))
}
pub async fn get_classification(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let e = entry(&state, id).await?;
    let stale = e.record.as_ref().is_none_or(|r| {
        r.source_json != e.source_json || r.classification.version != classification::VERSION
    });
    Ok(Json(
        serde_json::json!({"classification":e.record,"pending":stale}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub expected_revision: i64,
    #[serde(flatten)]
    pub overrides: Overrides,
}
pub async fn correct_classification(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(mut request): Json<Correction>,
) -> Result<Json<serde_json::Value>, ApiError> {
    request.overrides.validate().map_err(ApiError::BadRequest)?;
    let e = entry(&state, id).await?;
    let input = e.input()?;
    let record = e.record;
    let revision = record.as_ref().map(|r| r.revision).unwrap_or(0);
    if revision != request.expected_revision {
        return Err(ApiError::Conflict(
            "Classification changed; reload before correcting it.".into(),
        ));
    }
    let generated = record
        .filter(|r| r.source_json == e.source_json)
        .map(|r| r.classification)
        .unwrap_or_else(|| classification::classify(&input.metadata(), Vec::new()));
    let r = Record {
        source_json: e.source_json,
        classification: generated,
        overrides: request.overrides,
        revision,
    };
    if !state.store.write_classification(id, &r).await? {
        return Err(ApiError::Conflict(
            "Metadata changed; reload before correcting it.".into(),
        ));
    }
    Ok(Json(serde_json::json!({"revision":revision+1})))
}
pub async fn settings(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let enabled = state.store.get_setting(SEMANTIC_KEY).await?.as_deref() == Some("true");
    let status = observation().read().map(|v| v.clone()).unwrap_or_default();
    Ok(Json(
        serde_json::json!({"semantic_enabled":enabled,"classification":status,"semantic":semantic::status()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub semantic_enabled: bool,
}
pub async fn update_settings(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Json(request): Json<Settings>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .store
        .put_setting(
            SEMANTIC_KEY,
            if request.semantic_enabled {
                "true"
            } else {
                "false"
            },
        )
        .await?;
    if !request.semantic_enabled {
        semantic::disable();
    }
    Ok(Json(
        serde_json::json!({"semantic_enabled":request.semantic_enabled}),
    ))
}

#[derive(Deserialize)]
pub struct RelatedQuery {
    pub q: Option<String>,
}
pub async fn related_search(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<RelatedQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = semantic::related(&state, &query.q.unwrap_or_default()).await;
    let mut results = Vec::new();
    for id in ids {
        if let Some(item) = state.store.get_item(id).await? {
            results.push(crate::http::dto::ItemDto::from(item));
        }
    }
    Ok(Json(
        serde_json::json!({"results":results,"semantic":semantic::status()}),
    ))
}
