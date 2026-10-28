//! Managed local subtitle artifacts over the common durable job ownership.
pub(crate) mod api;
pub(crate) mod artifacts;
pub(crate) mod config;

use crate::{http::error::ApiError, state::AppState};
use artifacts::CatalogModel;
use plurx_core::{
    error::StoreError,
    store::background_jobs::{BackgroundJob, EnqueueJob, EnqueueOutcome, JobPayload, JobRequest},
    store::background_jobs_transcription::ModelAction,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Catalog {
    version: u8,
    pub(crate) revision: String,
    pub(crate) adapter_contract: String,
    pub(crate) models: Vec<CatalogModel>,
}
static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("catalog.json")).expect("reviewed subtitle catalog")
});
static CATALOG_V1: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("catalog-v1.json"))
        .expect("previous accepted subtitle catalog")
});
pub(crate) fn accepted_model_spec(
    revision: &str,
    id: &str,
    sha: &str,
    adapter: &str,
) -> Option<&'static CatalogModel> {
    let current = catalog();
    if adapter != current.adapter_contract {
        return None;
    }
    let model = current
        .models
        .iter()
        .find(|m| m.id == id && m.sha256 == sha)?;
    if revision == current.revision {
        return Some(model);
    }
    (revision == CATALOG_V1.revision
        && adapter == CATALOG_V1.adapter_contract
        && CATALOG_V1.models.iter().any(|old| old == model))
    .then_some(model)
}
pub(crate) fn catalog() -> &'static Catalog {
    &CATALOG
}
pub(crate) fn catalog_public() -> Value {
    let data = catalog();
    let models=data.models.iter().map(|model|json!({"id":model.id,"label":model.label,"languages":model.languages,"download_bytes":model.download_bytes,"sha256":model.sha256,"source":model.source,"license":model.license,"adapter_contract":data.adapter_contract})).collect::<Vec<_>>();
    json!({"version":data.version,"catalog_revision":data.revision,"models":models})
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EngineObservation {
    pub contract: String,
    pub platform: String,
    pub binary_sha256: String,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelObservation {
    pub model_id: String,
    pub model_sha256: String,
    pub status: String,
    pub verified_at_ms: Option<i64>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct JobWaitObservation {
    pub job_id: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkerObservation {
    pub node_id: String,
    pub label: String,
    pub eligible: bool,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
    pub engine: Option<EngineObservation>,
    pub models: Vec<ModelObservation>,
    pub waits: Vec<JobWaitObservation>,
}
const OBSERVATION_TTL: Duration = Duration::from_secs(60);

pub(crate) struct Runtime {
    pub(crate) translation: crate::subtitle_translation::Runtime,
    /// Persistent service data, never the disposable runtime-cache directory.
    pub root: PathBuf,
    pub boot_id: String,
    local: RwLock<Option<(Instant, WorkerObservation)>>,
    peers: RwLock<Option<(Instant, Vec<Value>)>>,
    waits: RwLock<BTreeMap<String, (Instant, String)>>,
    automatic_verifications: RwLock<BTreeMap<String, String>>,
    verification_failures: RwLock<std::collections::BTreeSet<String>>,
    candidate_cursor: RwLock<Option<plurx_core::store::background_jobs::CandidateCursor>>,
}
impl Runtime {
    pub(crate) fn new(data_dir: &Path) -> Self {
        let data_dir = if data_dir.as_os_str().is_empty() {
            PathBuf::new()
        } else if data_dir.is_absolute() {
            data_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(data_dir))
                .unwrap_or_default()
        };
        Self {
            translation: crate::subtitle_translation::Runtime::new(),
            root: if data_dir.is_absolute() {
                data_dir.join("subtitle-models")
            } else {
                PathBuf::new()
            },
            boot_id: uuid::Uuid::new_v4().to_string(),
            local: RwLock::new(None),
            peers: RwLock::new(None),
            waits: RwLock::new(BTreeMap::new()),
            automatic_verifications: RwLock::new(Default::default()),
            verification_failures: RwLock::new(Default::default()),
            candidate_cursor: RwLock::new(None),
        }
    }
    pub(crate) async fn local_observation(&self) -> Option<WorkerObservation> {
        let entry = self.local.read().await;
        entry
            .as_ref()
            .filter(|(when, _)| when.elapsed() <= OBSERVATION_TTL)
            .map(|(_, value)| value.clone())
    }
    async fn record_wait(&self, id: &str, reason: &str) {
        let mut rows = self.waits.write().await;
        rows.retain(|_, (when, _)| when.elapsed() <= OBSERVATION_TTL);
        if rows.len() >= 32 && !rows.contains_key(id) {
            rows.pop_first();
        }
        rows.insert(id.into(), (Instant::now(), reason.into()));
    }
}

fn unknown_worker(node_id: &str, label: &str) -> Value {
    json!({"node_id":node_id,"label":label,"eligible":false,"observed_at_ms":null,"expires_at_ms":null,"engine":null,"models":catalog().models.iter().map(|model|json!({"model_id":model.id,"status":"unknown","verified_at_ms":null,"reason":"worker_observation_unavailable"})).collect::<Vec<_>>()})
}
pub(crate) fn spoken_language_tag(tag: Option<&str>) -> Option<String> {
    let tag = tag?.trim();
    if tag.is_empty()
        || ["und", "unknown", "n/a"]
            .iter()
            .any(|unknown| tag.eq_ignore_ascii_case(unknown))
    {
        None
    } else {
        Some(crate::subtitle_transcription::normalized_language(tag))
    }
}

pub(crate) async fn workers_public(state: &AppState) -> Result<Vec<Value>, StoreError> {
    if let Some((when, rows)) = state.subtitle_models.peers.read().await.as_ref() {
        if when.elapsed() < Duration::from_secs(5) {
            return Ok(rows.clone());
        }
    }
    // Existing peer transport supplies bounded signed responses and fanout.
    let mut rows = Vec::new();
    rows.push(
        state
            .subtitle_models
            .local_observation()
            .await
            .map(|v| json!(v))
            .unwrap_or_else(|| unknown_worker(&state.node_id, &state.server_name)),
    );
    if let Ok(outcomes) = state.peer_activity.snapshots().await {
        for (node_id, outcome) in outcomes.iter().take(255) {
            if node_id == &state.node_id {
                continue;
            }
            let value = match outcome {
                crate::http::internal_activity::PeerActivityOutcome::Answered(snapshot) => snapshot
                    .workers
                    .as_ref()
                    .and_then(|workers| workers.subtitles.clone()),
                _ => None,
            };
            rows.push(
                value
                    .filter(|v| {
                        v.node_id == *node_id
                            && v.expires_at_ms > crate::state::clock_ms()
                            && v.observed_at_ms <= crate::state::clock_ms().saturating_add(5000)
                            && v.models.len() <= 4
                            && v.waits.len() <= 32
                    })
                    .map(|v| json!(v))
                    .unwrap_or_else(|| unknown_worker(node_id, "Unavailable worker")),
            );
        }
    }
    *state.subtitle_models.peers.write().await = Some((Instant::now(), rows.clone()));
    Ok(rows)
}

pub(crate) async fn job_wait_reason(
    state: &AppState,
    job: &BackgroundJob,
) -> Result<Option<String>, StoreError> {
    let payload = job.supported_payload()?;
    let model_operation = matches!(payload, JobPayload::SubtitleModelOperation { .. });
    if !model_operation {
        let setup = config::read(state.store.as_ref()).await?;
        if setup.mode_status == config::ModeStatus::ConfigurationConflict {
            return Ok(Some("configuration_conflict".into()));
        }
        if !setup.effective_enabled {
            return Ok(Some("disabled".into()));
        }
        if matches!(payload, JobPayload::SubtitleTranscribe { .. }) {
            return Ok(Some(
                if setup.mode == config::Mode::Advanced {
                    "waiting_for_worker"
                } else {
                    "legacy_profile_incompatible"
                }
                .into(),
            ));
        }
        if setup.mode != config::Mode::Managed {
            return Ok(Some("disabled".into()));
        }
    }
    let workers = workers_public(state).await?;
    for worker in &workers {
        if let Some(waits) = worker.get("waits").and_then(Value::as_array) {
            for wait in waits {
                if wait["job_id"].as_str() == Some(job.id.as_str()) {
                    if let Some(reason) = wait["reason"].as_str() {
                        return Ok(Some(reason.into()));
                    }
                }
            }
        }
    }
    if model_operation {
        return Ok(None);
    }
    let JobPayload::SubtitleTranscribeManaged { model_sha256, .. } = job.supported_payload()?
    else {
        return Ok(Some("legacy_profile_incompatible".into()));
    };
    if workers.iter().any(|worker| {
        worker["eligible"] == true
            && worker["models"].as_array().is_some_and(|models| {
                models.iter().any(|model| {
                    model["model_sha256"].as_str() == Some(&model_sha256)
                        && model["status"] == "verified"
                })
            })
    }) {
        Ok(Some("waiting_for_worker".into()))
    } else if workers.iter().any(|worker| {
        worker["engine"].is_null()
            || worker["models"]
                .as_array()
                .is_some_and(|models| models.iter().any(|model| model["status"] == "unknown"))
    }) {
        Ok(Some("worker_observation_unavailable".into()))
    } else {
        Ok(Some("no_compatible_worker".into()))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelOperationRequest {
    version: u8,
    node_ids: Vec<String>,
}
pub(crate) async fn install(
    _admin: crate::http::extract::AdminUser,
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(request): axum::Json<Value>,
) -> Result<axum::Json<Value>, ApiError> {
    if request["version"] == 2 {
        return crate::subtitle_translation::model_operation(
            &state,
            &id,
            ModelAction::TranslationInstall,
            request,
        )
        .await;
    }
    let request = serde_json::from_value(request)
        .map_err(|_| ApiError::BadRequest("Invalid model installation request".into()))?;
    model_operation(&state, &id, ModelAction::Install, request).await
}
pub(crate) async fn verify(
    _admin: crate::http::extract::AdminUser,
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(request): axum::Json<Value>,
) -> Result<axum::Json<Value>, ApiError> {
    if request["version"] == 2 {
        return crate::subtitle_translation::model_operation(
            &state,
            &id,
            ModelAction::TranslationVerify,
            request,
        )
        .await;
    }
    let request = serde_json::from_value(request)
        .map_err(|_| ApiError::BadRequest("Invalid model verification request".into()))?;
    model_operation(&state, &id, ModelAction::Verify, request).await
}
async fn model_operation(
    state: &AppState,
    id: &str,
    action: ModelAction,
    request: ModelOperationRequest,
) -> Result<axum::Json<Value>, ApiError> {
    let unique = request
        .node_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if request.version != 1
        || request.node_ids.is_empty()
        || request.node_ids.len() > 16
        || unique.len() != request.node_ids.len()
        || request
            .node_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
    {
        return Err(ApiError::BadRequest(
            "Choose one to sixteen distinct current workers with request version 1".into(),
        ));
    }
    let model = catalog()
        .models
        .iter()
        .find(|model| model.id == id)
        .ok_or(ApiError::NotFound("subtitle model"))?;
    let mut members = state
        .membership
        .activity_peers()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Worker membership unavailable".into()))?
        .into_iter()
        .map(|peer| peer.node_id)
        .collect::<std::collections::BTreeSet<_>>();
    members.insert(state.node_id.clone());
    let mut outcomes = Vec::new();
    for node_id in request.node_ids {
        if !members.contains(&node_id) {
            outcomes.push(json!({"node_id":node_id,"job_id":null,"outcome":"worker_not_current"}));
            continue;
        }
        let outcome = enqueue_model_operation(state, model, &node_id, action, None).await?;
        let job_id = match &outcome {
            EnqueueOutcome::Accepted { job_id, .. } | EnqueueOutcome::Existing { job_id, .. } => {
                Some(job_id.as_str())
            }
            _ => None,
        };
        let mut value = serde_json::to_value(&outcome)
            .map_err(|_| ApiError::Internal("Could not encode model operation".into()))?;
        value["node_id"] = json!(node_id);
        value["job_id"] = json!(job_id);
        outcomes.push(value);
    }
    Ok(axum::Json(
        json!({"version":1,"model_id":id,"operations":outcomes}),
    ))
}
async fn enqueue_model_operation(
    state: &AppState,
    model: &CatalogModel,
    node_id: &str,
    action: ModelAction,
    boot: Option<&str>,
) -> Result<EnqueueOutcome, StoreError> {
    let payload = JobPayload::SubtitleModelOperation {
        translation_profile: None,
        action,
        model_id: model.id.clone(),
        catalog_revision: catalog().revision.clone(),
        model_sha256: model.sha256.clone(),
        adapter_contract: catalog().adapter_contract.clone(),
        target_node_id: node_id.into(),
    };
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&payload).map_err(|e| StoreError::Task(e.to_string()))?,
    ));
    let now = crate::state::clock_ms();
    state
        .store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("subtitle-model:{digest}:{}", boot.unwrap_or("request")),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "subtitle-model".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: digest.clone(),
                consumer_kind: "subtitle_model_operation".into(),
                consumer_ref: digest,
                target_node_id: Some(node_id.into()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationRequest {
    version: u8,
    audio_index: i64,
    language: String,
    model_id: String,
    source_size: String,
    source_mtime: String,
}
pub(crate) async fn generate(
    _admin: crate::http::extract::AdminUser,
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> Result<axum::Json<Value>, ApiError> {
    let settings = config::read(state.store.as_ref()).await?;
    if !settings.effective_enabled {
        return Err(ApiError::Conflict(if settings.mode_status==config::ModeStatus::ConfigurationConflict{"Subtitle configuration changed in another administrator or older server; review Settings"}else{"Enable subtitle generation in Settings first"}.into()));
    }
    if settings.mode == config::Mode::Advanced && body == json!({}) {
        return crate::subtitle_transcription::enqueue(
            _admin,
            axum::extract::State(state),
            axum::extract::Path(id),
        )
        .await;
    }
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let request = if body == json!({}) {
        let language = crate::subtitle_transcription::normalized_language(&settings.language);
        let audio = file
            .audio_streams
            .iter()
            .find(|audio| {
                audio.default
                    && audio.language.as_deref().is_some_and(|tag| {
                        crate::subtitle_transcription::normalized_language(tag) == language
                    })
            })
            .or_else(|| {
                file.audio_streams.iter().find(|audio| {
                    audio.language.as_deref().is_some_and(|tag| {
                        crate::subtitle_transcription::normalized_language(tag) == language
                    })
                })
            })
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "Choose the audio track and its spoken language explicitly".into(),
                )
            })?;
        GenerationRequest {
            version: 1,
            audio_index: audio.index,
            language,
            model_id: settings.model_id.clone(),
            source_size: file.size.to_string(),
            source_mtime: file.mtime.to_string(),
        }
    } else {
        serde_json::from_value(body).map_err(|_| {
            ApiError::BadRequest("Use the supported subtitle request version and fields".into())
        })?
    };
    if request.version != 1
        || request.source_size != file.size.to_string()
        || request.source_mtime != file.mtime.to_string()
    {
        return Err(ApiError::Conflict(
            "The source changed; refresh subtitle options".into(),
        ));
    }
    if !crate::subtitle_transcription::eligible(&file) {
        return Err(ApiError::BadRequest(
            "Choose finite probed audio or video up to four hours".into(),
        ));
    }
    let language = crate::subtitle_transcription::normalized_language(&request.language);
    let audio = file
        .audio_streams
        .iter()
        .find(|audio| audio.index == request.audio_index)
        .ok_or_else(|| ApiError::BadRequest("Choose an existing audio track".into()))?;
    if spoken_language_tag(audio.language.as_deref()).is_some_and(|tag| tag != language) {
        return Err(ApiError::BadRequest(
            "The selected language disagrees with the audio tag".into(),
        ));
    }
    if settings.mode == config::Mode::Advanced {
        if request.model_id != "advanced" {
            return Err(ApiError::BadRequest(
                "Choose the configured Advanced model".into(),
            ));
        }
        let languages =
            crate::subtitle_transcription::model_languages(&settings.advanced.model_path).await;
        if !languages.contains(&language) {
            return Err(ApiError::BadRequest(
                "The configured Advanced model does not support that language".into(),
            ));
        }
        let (outcome, key) = crate::subtitle_transcription::enqueue_advanced_selected(
            &state,
            &file,
            request.audio_index,
            &language,
        )
        .await?;
        return generation_response(outcome, &key, &file);
    }
    let model = catalog()
        .models
        .iter()
        .find(|model| model.id == request.model_id)
        .ok_or_else(|| ApiError::BadRequest("Choose a catalog model".into()))?;
    if !model.languages.contains(&language) {
        return Err(ApiError::BadRequest(
            "The selected model does not support that language".into(),
        ));
    }
    let enqueue = generation_enqueue(&file, request.audio_index, &language, model)?;
    let key = enqueue.request.consumer_ref.clone();
    let outcome = state.store.enqueue_job(enqueue).await?;
    generation_response(outcome, &key, &file)
}
fn generation_enqueue(
    file: &plurx_core::domain::MediaFile,
    audio_index: i64,
    language: &str,
    model: &CatalogModel,
) -> Result<EnqueueJob, StoreError> {
    let payload = managed_payload(file, audio_index, language, model);
    let key = plurx_core::store::background_jobs_transcription::artifact_key(&payload)?;
    let now = crate::state::clock_ms();
    Ok(EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload,
        dedupe_key: format!("transcription:{key}"),
        priority: 0,
        not_before_ms: now,
        now_ms: now,
        request: JobRequest {
            scope: "subtitle-transcription".into(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: key.clone(),
            consumer_kind: "subtitle_transcribe_managed".into(),
            consumer_ref: key,
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    })
}

fn generation_response(
    outcome: EnqueueOutcome,
    key: &str,
    file: &plurx_core::domain::MediaFile,
) -> Result<axum::Json<Value>, ApiError> {
    let mut value = serde_json::to_value(outcome)
        .map_err(|_| ApiError::Internal("Could not encode subtitle request".into()))?;
    value["file_id"] = json!(file.id.to_string());
    value["artifact_key"] = json!(key);
    value["source_size"] = json!(file.size.to_string());
    value["source_mtime"] = json!(file.mtime.to_string());
    Ok(axum::Json(value))
}

// This recipe revision binds PCM silence/EOF normalization independently of
// the common engine adapter. Unreleased older recipe jobs are not reinterpreted.
const MANAGED_NORMALIZATION_REVISION: &str = "mono16k-s16-digital-zero-pcm-eof-v1";
fn managed_pipeline_digest(
    profile: &plurx_core::store::background_jobs_transcription::ManagedProfile,
    model: &CatalogModel,
    audio_index: i64,
    language: &str,
) -> String {
    hex::encode(Sha256::digest(format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        MANAGED_NORMALIZATION_REVISION,
        profile.adapter_contract,
        profile.catalog_revision,
        model.sha256,
        audio_index,
        language
    )))
}
fn managed_recipe_current(payload: &JobPayload, model: &CatalogModel) -> bool {
    match payload {
        JobPayload::SubtitleTranscribeManaged {
            profile,
            audio_index,
            language,
            pipeline_digest,
            ..
        } => pipeline_digest == &managed_pipeline_digest(profile, model, *audio_index, language),
        _ => false,
    }
}
fn managed_payload(
    file: &plurx_core::domain::MediaFile,
    audio_index: i64,
    language: &str,
    model: &CatalogModel,
) -> JobPayload {
    let profile = plurx_core::store::background_jobs_transcription::ManagedProfile {
        model_id: model.id.clone(),
        catalog_revision: catalog().revision.clone(),
        adapter_contract: catalog().adapter_contract.clone(),
    };
    let pipeline_digest = managed_pipeline_digest(&profile, model, audio_index, language);
    JobPayload::SubtitleTranscribeManaged {
        file_id: file.id,
        audio_index,
        source_size: file.size,
        source_mtime: file.mtime,
        language: language.into(),
        model_sha256: model.sha256.clone(),
        pipeline_digest,
        profile,
    }
}

impl WorkerObservation {
    pub(crate) fn bounded(&self, node: &str) -> bool {
        self.node_id == node
            && self.node_id.len() <= 256
            && self.label.len() <= 256
            && self.observed_at_ms >= 0
            && self.expires_at_ms >= self.observed_at_ms
            && self.expires_at_ms.saturating_sub(self.observed_at_ms) <= 60_000
            && self.models.len() <= 4
            && self.waits.len() <= 32
            && self.engine.as_ref().is_none_or(|engine| {
                engine.contract.len() <= 256
                    && engine.platform.len() <= 64
                    && engine.binary_sha256.len() <= 64
                    && matches!(
                        engine.status.as_str(),
                        "available" | "missing" | "unqualified"
                    )
            })
            && self.models.iter().all(|model| {
                model.model_id.len() <= 64
                    && model.model_sha256.len() == 64
                    && matches!(
                        model.status.as_str(),
                        "missing"
                            | "unverified_cache"
                            | "installed"
                            | "verified"
                            | "verification_failed"
                            | "unknown"
                    )
                    && model
                        .reason
                        .as_ref()
                        .is_none_or(|reason| reason.len() <= 64)
            })
            && self.waits.iter().all(|wait| {
                uuid::Uuid::parse_str(&wait.job_id).is_ok()
                    && matches!(
                        wait.reason.as_str(),
                        "waiting_for_playback_capacity"
                            | "no_compatible_worker"
                            | "engine_unavailable"
                            | "model_verification_failed"
                            | "worker_observation_unavailable"
                            | "worker_source_unavailable"
                    )
            })
    }
}
async fn refresh_local(state: &AppState) -> Result<(), StoreError> {
    let now = crate::state::clock_ms();
    let inspected = artifacts::inspect_engine().await;
    let engine = inspected.as_ref().ok();
    let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
    let mut models = Vec::new();
    for spec in &catalog().models {
        let opened = if state.subtitle_models.root.is_absolute() {
            artifacts::model_identity(&state.subtitle_models.root, spec).await
        } else {
            Err("model_store_unavailable".into())
        };
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.model_id == spec.id && receipt.model_sha256 == spec.sha256);
        let failed = state
            .subtitle_models
            .verification_failures
            .read()
            .await
            .contains(&spec.id);
        let accepted = opened.as_ref().ok().is_some_and(|identity| {
            accepted_model_identity(&state.node_id, spec, *identity, receipt)
        });
        let verified = accepted
            && !failed
            && opened.as_ref().ok().zip(engine).zip(receipt).is_some_and(
                |((model, engine), receipt)| {
                    receipt.boot_id == state.subtitle_models.boot_id
                        && receipt.adapter_contract == engine.adapter_contract
                        && receipt.engine_sha256 == engine.binary_sha256
                        && receipt.model_file_identity == artifacts::identity_key(*model)
                        && receipt.engine_file_identity == artifacts::identity_key(engine.identity)
                },
            );
        let status = if opened.is_ok() && !accepted {
            "unverified_cache"
        } else if failed {
            "verification_failed"
        } else if verified {
            "verified"
        } else if opened.is_ok() {
            "installed"
        } else if opened.as_ref().err().is_some_and(|error| {
            matches!(
                error.as_str(),
                "model_not_installed" | "model_store_unavailable"
            )
        }) {
            "missing"
        } else {
            "unknown"
        };
        models.push(ModelObservation {
            model_id: spec.id.clone(),
            model_sha256: spec.sha256.clone(),
            status: status.into(),
            verified_at_ms: verified.then(|| receipt.expect("verified receipt").verified_at_ms),
            reason: if status == "unknown" || status == "verification_failed" {
                Some("model_verification_failed".into())
            } else {
                None
            },
        });
        if !verified && !failed {
            if let Some((model, engine)) = opened.as_ref().ok().zip(engine) {
                requalify_accepted_model(state, spec, *model, engine.identity, receipt).await?;
            }
        }
    }
    let waits = state
        .subtitle_models
        .waits
        .read()
        .await
        .iter()
        .filter(|(_, (when, _))| when.elapsed() <= OBSERVATION_TTL)
        .map(|(job_id, (_, reason))| JobWaitObservation {
            job_id: job_id.clone(),
            reason: reason.clone(),
        })
        .collect();
    let observation = WorkerObservation {
        node_id: state.node_id.clone(),
        label: state.server_name.chars().take(64).collect(),
        eligible: state.serving.is_ready() && !state.membership.local_maintenance_active(),
        observed_at_ms: now,
        expires_at_ms: now.saturating_add(60_000),
        engine: Some(
            engine
                .map(|engine| EngineObservation {
                    contract: engine.adapter_contract.clone(),
                    platform: engine.platform.clone(),
                    binary_sha256: engine.binary_sha256.clone(),
                    status: "available".into(),
                })
                .unwrap_or_else(|| EngineObservation {
                    contract: catalog().adapter_contract.clone(),
                    platform: String::new(),
                    binary_sha256: String::new(),
                    status: if inspected
                        .as_ref()
                        .err()
                        .is_some_and(|reason| reason == "packaged_engine_missing")
                    {
                        "missing"
                    } else {
                        "unqualified"
                    }
                    .into(),
                }),
        ),
        models,
        waits,
    };
    *state.subtitle_models.local.write().await = Some((Instant::now(), observation));
    Ok(())
}

fn accepted_model_identity(
    node_id: &str,
    spec: &CatalogModel,
    identity: plurx_core::fs_secure::FileIdentity,
    receipt: Option<&plurx_core::store::background_jobs_transcription::SubtitleModelReceipt>,
) -> bool {
    receipt.is_some_and(|receipt| {
        receipt.node_id == node_id
            && receipt.model_id == spec.id
            && receipt.model_sha256 == spec.sha256
            && receipt.model_file_identity == artifacts::identity_key(identity)
    })
}

#[derive(Debug, PartialEq, Eq)]
enum ModelOperationOrigin<'a> {
    Explicit,
    Automatic {
        boot_id: &'a str,
        identity_sha: &'a str,
    },
}

fn model_operation_origin<'a>(
    job: &'a BackgroundJob,
    payload: &JobPayload,
) -> Option<ModelOperationOrigin<'a>> {
    let JobPayload::SubtitleModelOperation { action, .. } = payload else {
        return None;
    };
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(payload).ok()?));
    let prefix = format!("subtitle-model:{digest}:");
    let suffix = job.dedupe_key.strip_prefix(&prefix)?;
    if suffix == "request" {
        return Some(ModelOperationOrigin::Explicit);
    }
    let (boot_id, identity_sha) = suffix.split_once(':')?;
    (*action == ModelAction::Verify
        && uuid::Uuid::parse_str(boot_id).is_ok()
        && identity_sha.len() == 64
        && identity_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(ModelOperationOrigin::Automatic {
        boot_id,
        identity_sha,
    })
}

fn automatic_model_operation_current(
    state: &AppState,
    job: &BackgroundJob,
    payload: &JobPayload,
    spec: &CatalogModel,
    model: plurx_core::fs_secure::FileIdentity,
    engine: plurx_core::fs_secure::FileIdentity,
    receipt: Option<&plurx_core::store::background_jobs_transcription::SubtitleModelReceipt>,
) -> bool {
    let Some(ModelOperationOrigin::Automatic {
        boot_id,
        identity_sha,
    }) = model_operation_origin(job, payload)
    else {
        return false;
    };
    let key = format!(
        "{}:{}:{}",
        spec.id,
        artifacts::identity_key(model),
        artifacts::identity_key(engine)
    );
    boot_id == state.subtitle_models.boot_id
        && accepted_model_identity(&state.node_id, spec, model, receipt)
        && identity_sha == hex::encode(Sha256::digest(key.as_bytes()))
}

async fn retire_obsolete_model_operation(
    state: &AppState,
    job: &BackgroundJob,
    spec: &CatalogModel,
) -> Result<(), StoreError> {
    state
        .store
        .cancel_job(plurx_core::store::background_jobs::CancelJob {
            job_id: job.id.clone(),
            now_ms: crate::state::clock_ms(),
        })
        .await?;
    // A failed or stale queued attempt must not suppress a fresh current-boot
    // verification. Durable admission still coalesces an already-current job.
    state
        .subtitle_models
        .automatic_verifications
        .write()
        .await
        .remove(&spec.id);
    Ok(())
}

/// Only a successfully accepted installation may be requalified automatically.
/// A cancelled/crashed first install or replacement can leave immutable cache
/// bytes, but those bytes do not carry permission to finish the stopped work.
/// Explicit Install/Test may deliberately qualify them in its own fenced job.
async fn requalify_accepted_model(
    state: &AppState,
    spec: &CatalogModel,
    model: plurx_core::fs_secure::FileIdentity,
    engine: plurx_core::fs_secure::FileIdentity,
    receipt: Option<&plurx_core::store::background_jobs_transcription::SubtitleModelReceipt>,
) -> Result<bool, StoreError> {
    if !accepted_model_identity(&state.node_id, spec, model, receipt) {
        return Ok(false);
    }
    let key = format!(
        "{}:{}:{}",
        spec.id,
        artifacts::identity_key(model),
        artifacts::identity_key(engine)
    );
    schedule_automatic_verification(state, spec, &key).await
}

async fn schedule_automatic_verification(
    state: &AppState,
    spec: &CatalogModel,
    identity: &str,
) -> Result<bool, StoreError> {
    if state
        .subtitle_models
        .automatic_verifications
        .read()
        .await
        .get(&spec.id)
        .is_some_and(|saved| saved == identity)
    {
        return Ok(true);
    }
    let outcome = enqueue_model_operation(
        state,
        spec,
        &state.node_id,
        ModelAction::Verify,
        Some(&format!(
            "{}:{}",
            state.subtitle_models.boot_id,
            hex::encode(Sha256::digest(identity.as_bytes()))
        )),
    )
    .await?;
    let scheduled = matches!(
        outcome,
        EnqueueOutcome::Accepted { .. }
            | EnqueueOutcome::Existing {
                cancelled: false,
                ..
            }
    );
    if scheduled {
        state
            .subtitle_models
            .automatic_verifications
            .write()
            .await
            .insert(spec.id.clone(), identity.into());
    }
    Ok(scheduled)
}

struct PreparedSource {
    file: plurx_core::domain::MediaFile,
    held: crate::fragment_index_cluster::SourceFence,
}
enum SourceEligibility {
    Ready(Box<PreparedSource>),
    LocalUnavailable,
    Obsolete,
}
async fn prepare_generation_source(
    state: &AppState,
    payload: &JobPayload,
) -> Result<SourceEligibility, StoreError> {
    let JobPayload::SubtitleTranscribeManaged {
        file_id,
        source_size,
        source_mtime,
        ..
    } = payload
    else {
        return Ok(SourceEligibility::Obsolete);
    };
    let Some(file) = state.store.get_file(*file_id).await? else {
        return Ok(SourceEligibility::Obsolete);
    };
    if file.size != *source_size || file.mtime != *source_mtime {
        return Ok(SourceEligibility::Obsolete);
    }
    let Ok(held) = crate::fragment_index_cluster::open_source_playback_fence(&file, None).await
    else {
        return Ok(SourceEligibility::LocalUnavailable);
    };
    Ok(SourceEligibility::Ready(Box::new(PreparedSource {
        file,
        held,
    })))
}

const MANAGED_SCHEDULE_KEY: &str = "subtitles.transcription.managed.schedule.v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Schedule {
    version: u8,
    configuration_revision: String,
    next_run_ms: i64,
    cursor: i64,
}
impl Schedule {
    fn current(raw: Option<&str>, revision: &str) -> Self {
        raw.filter(|raw| raw.len() <= 512)
            .and_then(|raw| serde_json::from_str::<Self>(raw).ok())
            .filter(|value| {
                value.version == 1
                    && value.configuration_revision == revision
                    && value.next_run_ms >= 0
                    && value.cursor >= 0
            })
            .unwrap_or_else(|| Self {
                version: 1,
                configuration_revision: revision.into(),
                next_run_ms: 0,
                cursor: 0,
            })
    }
    fn encoded(&self) -> Result<String, StoreError> {
        serde_json::to_string(self).map_err(|error| StoreError::Task(error.to_string()))
    }
}
async fn next_candidates(
    state: &AppState,
    kinds: Vec<plurx_core::store::background_jobs::JobKind>,
) -> Result<plurx_core::store::background_jobs::CandidatePage, StoreError> {
    let mut cursor = state.subtitle_models.candidate_cursor.write().await;
    let page = state
        .store
        .job_candidates(plurx_core::store::background_jobs::CandidateQuery {
            node_id: state.node_id.clone(),
            kinds,
            after: cursor.clone(),
            now_ms: crate::state::clock_ms(),
            limit: 32,
        })
        .await?;
    *cursor = page.next.clone();
    Ok(page)
}

async fn automatic_page(
    state: &AppState,
    settings: &config::Snapshot,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<(), StoreError> {
    let Some(model) = catalog().models.iter().find(|model| {
        model.id == settings.model_id && model.languages.contains(&settings.language)
    }) else {
        return Ok(());
    };
    if settings.mode != config::Mode::Managed
        || !settings.effective_enabled
        || settings.interval_mins == 0
    {
        return Ok(());
    }
    let now = crate::state::clock_ms();
    let raw = state.store.get_setting(MANAGED_SCHEDULE_KEY).await?;
    let mut schedule = Schedule::current(raw.as_deref(), &settings.revision);
    if schedule.next_run_ms > now {
        return Ok(());
    }
    schedule.next_run_ms = now.saturating_add(i64::from(settings.interval_mins) * 60_000);
    let publisher = lease.publisher(state.store.as_ref());
    publisher
        .put_setting(MANAGED_SCHEDULE_KEY, &schedule.encoded()?)
        .await?;
    let ids = state
        .store
        .subtitle_candidate_file_ids(schedule.cursor, 8)
        .await?;
    for id in &ids {
        let current = config::read(state.store.as_ref()).await?;
        if current.mode != config::Mode::Managed
            || !current.effective_enabled
            || current.revision != settings.revision
        {
            break;
        }
        if let Some(file) = state.store.get_file(*id).await? {
            if crate::subtitle_transcription::eligible(&file)
                && !file.subtitle_streams.iter().any(|track| {
                    spoken_language_tag(track.language.as_deref()).as_deref()
                        == Some(settings.language.as_str())
                })
            {
                let matching = |track: &&plurx_core::domain::AudioStream| {
                    spoken_language_tag(track.language.as_deref()).as_deref()
                        == Some(settings.language.as_str())
                };
                if let Some(audio) = file
                    .audio_streams
                    .iter()
                    .filter(matching)
                    .find(|track| track.default)
                    .or_else(|| file.audio_streams.iter().find(matching))
                {
                    let _ = publisher
                        .enqueue_transcription(generation_enqueue(
                            &file,
                            audio.index,
                            &settings.language,
                            model,
                        )?)
                        .await?;
                }
            }
        }
        schedule.cursor = *id;
        publisher
            .put_setting(MANAGED_SCHEDULE_KEY, &schedule.encoded()?)
            .await?;
    }
    if ids.is_empty() {
        schedule.cursor = 0;
        publisher
            .put_setting(MANAGED_SCHEDULE_KEY, &schedule.encoded()?)
            .await?;
    }
    Ok(())
}

pub(crate) async fn worker_pass(
    state: &AppState,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(), StoreError> {
    refresh_local(state).await?;
    use plurx_core::store::background_jobs::{ClaimJob, JobKind, JobSettlement};
    let authority = state.jobs.execution_authority();
    let settings = config::read(state.store.as_ref()).await?;
    if settings.mode == config::Mode::Managed
        && settings.effective_enabled
        && settings.interval_mins > 0
        && authority
            .may_execute_job(JobKind::SubtitleTranscribeManaged)
            .await
    {
        if let Some(lease) = state
            .jobs
            .acquire_job("subtitle-transcription:sweep".into())
            .await?
        {
            let result = automatic_page(state, &settings, &lease).await;
            lease.release().await?;
            result?;
        }
    }
    let mut kinds = vec![JobKind::SubtitleModelOperation];
    if settings.mode == config::Mode::Managed && settings.effective_enabled {
        kinds.push(JobKind::SubtitleTranscribeManaged);
    }
    let candidates = next_candidates(state, kinds).await?;
    for candidate in candidates.jobs {
        let payload = candidate.supported_payload()?;
        if matches!(&payload,JobPayload::SubtitleModelOperation{action,..} if action.is_translation())
        {
            continue;
        }
        let kind = payload.kind();
        if !authority.may_execute_job(kind).await || shutdown.is_cancelled() {
            continue;
        }
        let spec = match &payload {
            JobPayload::SubtitleModelOperation {
                model_id,
                catalog_revision,
                model_sha256,
                adapter_contract,
                ..
            } => accepted_model_spec(catalog_revision, model_id, model_sha256, adapter_contract),
            JobPayload::SubtitleTranscribeManaged {
                model_sha256,
                profile,
                ..
            } => accepted_model_spec(
                &profile.catalog_revision,
                &profile.model_id,
                model_sha256,
                &profile.adapter_contract,
            ),
            _ => None,
        };
        let Some(spec) = spec else {
            state
                .subtitle_models
                .record_wait(&candidate.id, "no_compatible_worker")
                .await;
            continue;
        };
        if matches!(payload, JobPayload::SubtitleTranscribeManaged { .. })
            && !managed_recipe_current(&payload, spec)
        {
            state
                .subtitle_models
                .record_wait(&candidate.id, "no_compatible_worker")
                .await;
            continue;
        }
        let generation = matches!(payload, JobPayload::SubtitleTranscribeManaged { .. });
        let automatic_model_identity = if !generation {
            match model_operation_origin(&candidate, &payload) {
                Some(ModelOperationOrigin::Explicit) => None,
                Some(ModelOperationOrigin::Automatic { boot_id, .. }) => {
                    let model = artifacts::model_identity(&state.subtitle_models.root, spec)
                        .await
                        .ok();
                    let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
                    let receipt = receipts.iter().find(|receipt| receipt.model_id == spec.id);
                    if boot_id != state.subtitle_models.boot_id
                        || !model.is_some_and(|model| {
                            accepted_model_identity(&state.node_id, spec, model, receipt)
                        })
                    {
                        retire_obsolete_model_operation(state, &candidate, spec).await?;
                        continue;
                    }
                    model
                }
                None => {
                    retire_obsolete_model_operation(state, &candidate, spec).await?;
                    continue;
                }
            }
        } else {
            None
        };
        let engine = match artifacts::inspect_engine().await {
            Ok(engine) => engine,
            Err(_) => {
                state
                    .subtitle_models
                    .record_wait(&candidate.id, "engine_unavailable")
                    .await;
                continue;
            }
        };
        if let Some(model) = automatic_model_identity {
            let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
            let receipt = receipts.iter().find(|receipt| receipt.model_id == spec.id);
            if !automatic_model_operation_current(
                state,
                &candidate,
                &payload,
                spec,
                model,
                engine.identity,
                receipt,
            ) {
                retire_obsolete_model_operation(state, &candidate, spec).await?;
                continue;
            }
        }
        let model_identity = if generation {
            let model = match artifacts::model_identity(&state.subtitle_models.root, spec).await {
                Ok(model) => model,
                Err(_) => {
                    state
                        .subtitle_models
                        .record_wait(&candidate.id, "no_compatible_worker")
                        .await;
                    continue;
                }
            };
            let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
            if state
                .subtitle_models
                .verification_failures
                .read()
                .await
                .contains(&spec.id)
                || !receipts.iter().any(|receipt| {
                    receipt.model_id == spec.id
                        && receipt.model_sha256 == spec.sha256
                        && receipt.boot_id == state.subtitle_models.boot_id
                        && receipt.engine_sha256 == engine.binary_sha256
                        && receipt.adapter_contract == engine.adapter_contract
                        && receipt.model_file_identity == artifacts::identity_key(model)
                        && receipt.engine_file_identity == artifacts::identity_key(engine.identity)
                })
            {
                state
                    .subtitle_models
                    .record_wait(&candidate.id, "model_verification_failed")
                    .await;
                continue;
            }
            Some(model)
        } else {
            None
        };
        let prepared_source = if generation {
            match prepare_generation_source(state, &payload).await? {
                SourceEligibility::Ready(source) => Some(source),
                SourceEligibility::LocalUnavailable => {
                    state
                        .subtitle_models
                        .record_wait(&candidate.id, "worker_source_unavailable")
                        .await;
                    continue;
                }
                SourceEligibility::Obsolete => {
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
        } else {
            None
        };
        let admission = if generation {
            match state.transcode.admit_fragment().await {
                Some(admission) if state.transcode.fragment_worker_idle(&admission) => {
                    Some(admission)
                }
                _ => {
                    state
                        .subtitle_models
                        .record_wait(&candidate.id, "waiting_for_playback_capacity")
                        .await;
                    continue;
                }
            }
        } else {
            None
        };
        let now = crate::state::clock_ms();
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
        let active = crate::background_jobs::ActiveBackgroundJob::start(
            std::sync::Arc::clone(&state.store),
            std::sync::Arc::clone(&authority),
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("subtitle claim token missing".into()))?,
            deadline,
            kind,
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let operation = async {
            if let JobPayload::SubtitleModelOperation { action, .. } = &payload {
                if !fence
                    .observe_subtitle(if *action == ModelAction::Install {
                        plurx_core::store::background_jobs_transcription::SubtitlePhase::Downloading
                    } else {
                        plurx_core::store::background_jobs_transcription::SubtitlePhase::Verifying
                    })
                    .await?
                {
                    return Err(StoreError::Task("subtitle_operation_fenced".into()));
                }
                let preadmission = if *action == ModelAction::Verify {
                    Some(admit_model_test(state, &fence, &cancel).await?)
                } else {
                    None
                };
                let model = match action {
                    ModelAction::Install => {
                        artifacts::install_model(&state.subtitle_models.root, spec, &cancel)
                            .await
                            .map_err(StoreError::Task)?
                    }
                    ModelAction::Verify => {
                        hash_admitted_model(
                            state,
                            spec,
                            &cancel,
                            preadmission.as_ref().expect("verification admission"),
                        )
                        .await?
                    }
                    _ => {
                        return Err(StoreError::Task(
                            "translation model operation owned by translation worker".into(),
                        ))
                    }
                };
                if automatic_model_identity.is_some_and(|accepted| accepted != model.identity) {
                    return Err(StoreError::Task("automatic_verification_obsolete".into()));
                }
                let admission = match preadmission {
                    Some(admission) => admission,
                    None => admit_model_test(state, &fence, &cancel).await?,
                };
                if !state.transcode.speech_capacity_available(&admission) {
                    return Err(StoreError::Task("waiting_for_playback_capacity".into()));
                }
                if !fence
                    .observe_subtitle(
                        plurx_core::store::background_jobs_transcription::SubtitlePhase::Verifying,
                    )
                    .await?
                {
                    return Err(StoreError::Task("subtitle_operation_fenced".into()));
                }
                crate::subtitle_transcription::self_test_managed(
                    state, &engine, &model, &fence, &admission, &cancel,
                )
                .await?;
                if cancel.is_cancelled() {
                    return Err(StoreError::Task("worker_interrupted".into()));
                }
                let named = artifacts::model_identity(&state.subtitle_models.root, spec)
                    .await
                    .map_err(StoreError::Task)?;
                if named != model.identity {
                    return Err(StoreError::Task(
                        if automatic_model_identity.is_some() {
                            "automatic_verification_obsolete"
                        } else {
                            "artifact_changed"
                        }
                        .into(),
                    ));
                }
                let receipt =
                    plurx_core::store::background_jobs_transcription::SubtitleModelReceipt {
                        node_id: String::new(),
                        boot_id: String::new(),
                        model_id: spec.id.clone(),
                        model_sha256: spec.sha256.clone(),
                        adapter_contract: engine.adapter_contract.clone(),
                        engine_sha256: engine.binary_sha256.clone(),
                        model_file_identity: artifacts::identity_key(model.identity),
                        engine_file_identity: artifacts::identity_key(engine.identity),
                        job_id: String::new(),
                        fence: 0,
                        verified_at_ms: 0,
                    };
                fence.publish_subtitle_model(payload.clone(), receipt).await
            } else if let JobPayload::SubtitleTranscribeManaged {
                file_id,
                source_size,
                source_mtime,
                ..
            } = &payload
            {
                let source = prepared_source
                    .as_ref()
                    .expect("preclaim source eligibility");
                let current = state
                    .store
                    .get_file(*file_id)
                    .await?
                    .ok_or_else(|| StoreError::Task("source_changed".into()))?;
                if current.size != *source_size || current.mtime != *source_mtime {
                    return Err(StoreError::Task("source_changed".into()));
                }
                if !source.held.unchanged()
                    || crate::fragment_index_cluster::open_source_playback_fence(
                        &source.file,
                        Some(source.held.object_version()),
                    )
                    .await
                    .is_err()
                {
                    return Err(StoreError::Task("worker_source_unavailable".into()));
                }
                let file = &source.file;
                let model = hash_admitted_model(
                    state,
                    spec,
                    &cancel,
                    admission.as_ref().expect("generation admission"),
                )
                .await?;
                if Some(model.identity) != model_identity {
                    return Err(StoreError::Task("worker_interrupted".into()));
                }
                let caption = crate::subtitle_transcription::transcribe_managed(
                    state,
                    file,
                    &payload,
                    &engine,
                    &model,
                    &fence,
                    admission.as_ref().expect("generation admission"),
                    &cancel,
                )
                .await?;
                if !fence
                    .observe_subtitle(
                        plurx_core::store::background_jobs_transcription::SubtitlePhase::Publishing,
                    )
                    .await?
                {
                    return Err(StoreError::Task("subtitle_operation_fenced".into()));
                }
                fence.publish_transcription(payload.clone(), caption).await
            } else {
                Ok(false)
            }
        };
        let operation = plurx_core::process::bounded::cancellable(cancel.clone(), operation);
        tokio::pin!(operation);
        let result = loop {
            tokio::select! {
                result=&mut operation=>break result,
                ()=shutdown.cancelled()=>{cancel.cancel();break operation.await;},
                ()=tokio::time::sleep(Duration::from_secs(15))=>{let _=tokio::time::timeout(Duration::from_secs(2),refresh_local(state)).await;}
            }
        };
        match result {
            Ok(true) => {
                if !generation {
                    state
                        .subtitle_models
                        .verification_failures
                        .write()
                        .await
                        .remove(&spec.id);
                }
            }
            Ok(false) => {
                let _ = fence
                    .settle(JobSettlement::Stop {
                        error_code: "subtitle_publication_refused".into(),
                    })
                    .await;
            }
            Err(error) => {
                let reason = error.to_string();
                if reason.contains("automatic_verification_obsolete") {
                    let _ = fence
                        .settle(JobSettlement::Stop {
                            error_code: "automatic_verification_obsolete".into(),
                        })
                        .await;
                    state
                        .subtitle_models
                        .automatic_verifications
                        .write()
                        .await
                        .remove(&spec.id);
                } else if cancel.is_cancelled()
                    || shutdown.is_cancelled()
                    || reason.contains("waiting_for_playback_capacity")
                    || reason.contains("worker_interrupted")
                    || reason.contains("worker_source_unavailable")
                {
                    let _ = fence
                        .settle(JobSettlement::Yield {
                            error_code: Some(
                                if reason.contains("worker_source_unavailable") {
                                    "worker_source_unavailable"
                                } else if reason.contains("playback") {
                                    "waiting_for_playback_capacity"
                                } else {
                                    "worker_interrupted"
                                }
                                .into(),
                            ),
                            checkpoint: None,
                            not_before_ms: crate::state::clock_ms().saturating_add(5000),
                        })
                        .await;
                } else {
                    if !generation {
                        state
                            .subtitle_models
                            .verification_failures
                            .write()
                            .await
                            .insert(spec.id.clone());
                    }
                    let _ = fence
                        .settle(JobSettlement::Fail {
                            error_code: safe_operation_error(&reason).into(),
                        })
                        .await;
                }
            }
        }
        active.finish().await;
        refresh_local(state).await?;
        return Ok(());
    }
    Ok(())
}
async fn admit_model_test(
    state: &AppState,
    fence: &crate::background_jobs::JobFence,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<crate::transcode::FragmentAdmission, StoreError> {
    use plurx_core::store::background_jobs_transcription::SubtitlePhase;
    if !fence
        .observe_subtitle(SubtitlePhase::WaitingForCapacity)
        .await?
    {
        return Err(StoreError::Task("subtitle_operation_fenced".into()));
    }
    let admission = state
        .transcode
        .admit_speech_queued(cancel)
        .await
        .ok_or_else(|| StoreError::Task("waiting_for_playback_capacity".into()))?;
    if cancel.is_cancelled() {
        return Err(StoreError::Task("worker_interrupted".into()));
    }
    if !fence.observe_subtitle(SubtitlePhase::Verifying).await? {
        return Err(StoreError::Task("subtitle_operation_fenced".into()));
    }
    Ok(admission)
}

async fn hash_admitted_model(
    state: &AppState,
    spec: &CatalogModel,
    cancel: &tokio_util::sync::CancellationToken,
    admission: &crate::transcode::FragmentAdmission,
) -> Result<artifacts::ManagedModel, StoreError> {
    if !state.transcode.speech_capacity_available(admission) {
        return Err(StoreError::Task("waiting_for_playback_capacity".into()));
    }
    let operation = artifacts::open_model_cancellable(&state.subtitle_models.root, spec, cancel);
    tokio::pin!(operation);
    loop {
        tokio::select! {
            result=&mut operation=>return result.map_err(StoreError::Task),
            ()=tokio::time::sleep(Duration::from_millis(250))=>{
                if cancel.is_cancelled() || !state.transcode.speech_capacity_available(admission) {
                    let reason = if cancel.is_cancelled() { "worker_interrupted" } else { "waiting_for_playback_capacity" };
                    cancel.cancel();
                    let _ = operation.await;
                    return Err(StoreError::Task(reason.into()));
                }
            }
        }
    }
}

fn safe_operation_error(reason: &str) -> &'static str {
    let code = reason.rsplit(": ").next().unwrap_or(reason);
    if [
        "decode_failed",
        "no_speech_detected",
        "inference_failed",
        "inference_timeout",
        "output_invalid",
        "output_too_large",
        "source_changed",
        "worker_interrupted",
    ]
    .contains(&code)
    {
        return match code {
            "decode_failed" => "decode_failed",
            "no_speech_detected" => "no_speech_detected",
            "inference_failed" => "inference_failed",
            "inference_timeout" => "inference_timeout",
            "output_invalid" => "output_invalid",
            "output_too_large" => "output_too_large",
            "source_changed" => "source_changed",
            _ => "worker_interrupted",
        };
    }
    if reason.contains("disk") || reason.contains("reservation") {
        "insufficient_disk"
    } else if reason.contains("download") || reason.contains("redirect") {
        "model_download_failed"
    } else if reason.contains("engine") {
        "engine_unavailable"
    } else {
        "model_verification_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{
        background_jobs::{CancelJob, ClaimJob, ClaimOutcome, JobState},
        SqliteStore,
    };
    fn state(root: &Path) -> AppState {
        AppState::new(
            "Subtitle manager test".into(),
            std::sync::Arc::new(SqliteStore::open_in_memory().expect("SQLite")),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "worker-one".into(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }
    fn persistent_state(root: &Path, database: &Path) -> AppState {
        let mut state = state(root);
        state.store = std::sync::Arc::new(SqliteStore::open(database).expect("persistent SQLite"));
        state.subtitle_models = std::sync::Arc::new(Runtime::new(root));
        state
    }

    async fn model_receipt_fixture(
        state: &AppState,
        identity: plurx_core::fs_secure::FileIdentity,
        action: ModelAction,
        cancelled: bool,
    ) {
        use plurx_core::store::{
            background_jobs::{JobKind, JobSettlement, SettleJob},
            background_jobs_transcription::{PublishSubtitleModel, SubtitleModelReceipt},
        };
        let spec = &catalog().models[0];
        let outcome = enqueue_model_operation(state, spec, &state.node_id, action, None)
            .await
            .expect("explicit operation");
        let EnqueueOutcome::Accepted { job_id, .. } = outcome else {
            panic!("fresh explicit operation: {outcome:?}");
        };
        let queued = state
            .store
            .background_job(&job_id)
            .await
            .expect("read")
            .expect("job");
        let now = crate::state::clock_ms();
        let ClaimOutcome::Claimed { job } = state
            .store
            .claim_artifact_job(ClaimJob {
                job_id: job_id.clone(),
                expected_revision: queued.revision,
                node_id: state.node_id.clone(),
                boot_id: state.subtitle_models.boot_id.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleModelOperation,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("claim")
        else {
            panic!("owned model operation");
        };
        let source = job.supported_payload().expect("typed operation");
        let token = job.token.expect("owned token");
        let receipt = SubtitleModelReceipt {
            node_id: state.node_id.clone(),
            boot_id: state.subtitle_models.boot_id.clone(),
            model_id: spec.id.clone(),
            model_sha256: spec.sha256.clone(),
            adapter_contract: catalog().adapter_contract.clone(),
            engine_sha256: "e".repeat(64),
            model_file_identity: artifacts::identity_key(identity),
            engine_file_identity: "engine-original".into(),
            job_id: job_id.clone(),
            fence: token.fence,
            verified_at_ms: now + 1,
        };
        if cancelled {
            // Refresh can run while Install owns bytes but before it has a receipt.
            let receipts = state
                .store
                .subtitle_model_receipts(&state.node_id)
                .await
                .expect("receipts");
            let prior = receipts.iter().find(|receipt| receipt.model_id == spec.id);
            assert!(
                !requalify_accepted_model(state, spec, identity, identity, prior)
                    .await
                    .expect("refresh during install")
            );
            let stopped = state
                .store
                .cancel_job(CancelJob {
                    job_id,
                    now_ms: now + 1,
                })
                .await
                .expect("Stop")
                .expect("stopped job");
            assert!(!state
                .store
                .publish_subtitle_model(PublishSubtitleModel {
                    translation: None,
                    token,
                    source,
                    receipt,
                    now_ms: now + 2,
                })
                .await
                .expect("fenced ready publication"));
            assert!(state
                .store
                .settle_job(SettleJob {
                    token: stopped.token.expect("current cancelling token"),
                    settlement: JobSettlement::Cancel,
                    now_ms: now + 2,
                })
                .await
                .expect("retire stopped attempt"));
        } else {
            assert!(state
                .store
                .publish_subtitle_model(PublishSubtitleModel {
                    translation: None,
                    token,
                    source,
                    receipt,
                    now_ms: now + 1,
                })
                .await
                .expect("successful fenced receipt"));
        }
    }

    #[test]
    fn additive_small_catalog_preserves_only_exact_prior_generation_model_tuples() {
        let previous = &*CATALOG_V1;
        for old in &previous.models {
            assert_eq!(
                accepted_model_spec(
                    &previous.revision,
                    &old.id,
                    &old.sha256,
                    &previous.adapter_contract
                ),
                Some(old)
            );
            assert!(accepted_model_spec(
                &previous.revision,
                &old.id,
                &"f".repeat(64),
                &previous.adapter_contract
            )
            .is_none());
            assert!(accepted_model_spec(
                "unknown",
                &old.id,
                &old.sha256,
                &previous.adapter_contract
            )
            .is_none());
        }
        let small = catalog()
            .models
            .iter()
            .find(|m| m.id == "small")
            .expect("appended model");
        assert!(accepted_model_spec(
            &previous.revision,
            &small.id,
            &small.sha256,
            &previous.adapter_contract
        )
        .is_none());
        assert_eq!(small, &crate::subtitle_translation::profile::model_spec());
        assert!(serde_json::from_value::<ModelOperationRequest>(
            json!({"version":1,"node_ids":["worker"],"purpose":"translation"})
        )
        .is_err());
    }

    #[tokio::test]
    async fn cancelled_install_cache_cannot_requalify_after_restart_or_replacement() {
        use plurx_core::fs_secure::std_file_identity;
        let root = crate::test_tempdir().expect("private root");
        let database = root.path().join("eligibility.sqlite");
        let spec = &catalog().models[0];
        let model_root = root.path().join("subtitle-models");
        std::fs::create_dir(&model_root).expect("model root");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&model_root, std::fs::Permissions::from_mode(0o700))
            .expect("private models");
        let cached_path = model_root.join(format!("{}.bin", spec.sha256));
        // Only filesystem identity and durable receipt eligibility are under
        // test here; these bytes do not claim to be a recognizer model.
        let cache = std::fs::File::create(&cached_path).expect("cache");
        cache
            .set_len(spec.download_bytes)
            .expect("sparse metadata fixture");
        cache
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .expect("model permissions");
        drop(cache);
        let held = std::fs::File::open(&cached_path).expect("held original cache");
        let original = std_file_identity(&held).expect("identity");
        let initial = persistent_state(root.path(), &database);
        model_receipt_fixture(&initial, original, ModelAction::Install, true).await;
        assert!(initial
            .store
            .subtitle_model_receipts(&initial.node_id)
            .await
            .expect("receipts")
            .is_empty());
        initial.shutdown.cancel();
        drop(initial);

        let rebooted = persistent_state(root.path(), &database);
        assert!(
            !requalify_accepted_model(&rebooted, spec, original, original, None)
                .await
                .expect("cancelled cache stays ineligible after actual SQLite reopen")
        );
        assert!(rebooted
            .subtitle_models
            .automatic_verifications
            .read()
            .await
            .is_empty());
        // A fresh explicit Test deliberately qualifies already downloaded cache.
        model_receipt_fixture(&rebooted, original, ModelAction::Verify, false).await;
        let accepted_boot = rebooted.subtitle_models.boot_id.clone();
        rebooted.shutdown.cancel();
        drop(rebooted);

        let next_boot = persistent_state(root.path(), &database);
        assert_ne!(next_boot.subtitle_models.boot_id, accepted_boot);
        let receipts = next_boot
            .store
            .subtitle_model_receipts(&next_boot.node_id)
            .await
            .expect("durable receipt");
        let prior = receipts.first().expect("successful explicit Test receipt");
        assert!(accepted_model_identity(
            &next_boot.node_id,
            spec,
            original,
            Some(prior)
        ));
        let changed_engine = plurx_core::fs_secure::FileIdentity {
            inode: original.inode.saturating_add(1),
            ..original
        };
        assert!(
            requalify_accepted_model(&next_boot, spec, original, changed_engine, Some(prior))
                .await
                .expect(
                    "successful unchanged installation requalifies across boot and engine change"
                )
        );
        let candidates = next_boot
            .store
            .job_candidates(plurx_core::store::background_jobs::CandidateQuery {
                node_id: next_boot.node_id.clone(),
                kinds: vec![plurx_core::store::background_jobs::JobKind::SubtitleModelOperation],
                after: None,
                now_ms: crate::state::clock_ms() + 10,
                limit: 32,
            })
            .await
            .expect("automatic Verify");
        assert_eq!(candidates.jobs.len(), 1);
        let auto = candidates.jobs[0].clone();
        assert!(matches!(
            auto.supported_payload().expect("automatic payload"),
            JobPayload::SubtitleModelOperation {
                action: ModelAction::Verify,
                ..
            }
        ));
        let automatic_payload = auto.supported_payload().expect("automatic payload");
        assert!(automatic_model_operation_current(
            &next_boot,
            &auto,
            &automatic_payload,
            spec,
            original,
            changed_engine,
            Some(prior)
        ));
        let mut old_boot_auto = auto.clone();
        old_boot_auto.dedupe_key = auto
            .dedupe_key
            .replace(&next_boot.subtitle_models.boot_id, &accepted_boot);
        assert!(
            !automatic_model_operation_current(
                &next_boot,
                &old_boot_auto,
                &automatic_payload,
                spec,
                original,
                changed_engine,
                Some(prior)
            ),
            "old-boot automatic work cannot claim unchanged accepted bytes"
        );
        let mut malformed = auto.clone();
        malformed.dedupe_key.push_str(":extra");
        assert!(model_operation_origin(&malformed, &automatic_payload).is_none());
        malformed.dedupe_key = "subtitle-model:not-the-payload-digest:request".into();
        assert!(model_operation_origin(&malformed, &automatic_payload).is_none());
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&automatic_payload).expect("typed payload"),
        ));
        malformed.dedupe_key = format!("subtitle-model:{digest}:request");
        assert_eq!(
            model_operation_origin(&malformed, &automatic_payload),
            Some(ModelOperationOrigin::Explicit)
        );
        malformed.dedupe_key = format!("subtitle-model:{digest}:not-a-boot:{}", "a".repeat(64));
        assert!(model_operation_origin(&malformed, &automatic_payload).is_none());

        // Retaining the old descriptor prevents inode reuse. Equal-length new
        // bytes cannot inherit the successful old receipt just through a name.
        let replacement = root.path().join("replacement.bin");
        let replacement_file = std::fs::File::create(&replacement).expect("replacement");
        replacement_file
            .set_len(spec.download_bytes)
            .expect("same-length replacement");
        replacement_file
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .expect("replacement permissions");
        drop(replacement_file);
        std::fs::rename(&replacement, &cached_path).expect("replace path");
        let replaced = std_file_identity(&std::fs::File::open(&cached_path).expect("new cache"))
            .expect("new identity");
        assert_ne!(original, replaced);
        model_receipt_fixture(&next_boot, replaced, ModelAction::Install, true).await;
        assert_eq!(
            next_boot
                .store
                .background_job(&auto.id)
                .await
                .expect("auto job")
                .expect("auto job")
                .state,
            JobState::Queued,
            "auto A remains queued throughout the cancelled replacement B"
        );
        assert!(
            !automatic_model_operation_current(
                &next_boot,
                &auto,
                &automatic_payload,
                spec,
                replaced,
                changed_engine,
                Some(prior)
            ),
            "queued automatic A cannot qualify cancelled replacement B"
        );
        worker_pass(&next_boot, &next_boot.shutdown)
            .await
            .expect("real worker retires queued stale auto A before engine access");
        assert_eq!(
            next_boot
                .store
                .background_job(&auto.id)
                .await
                .expect("retired auto")
                .expect("retired auto")
                .state,
            JobState::Cancelled
        );
        let observation = next_boot
            .subtitle_models
            .local_observation()
            .await
            .expect("worker refresh");
        let orphan = observation
            .models
            .iter()
            .find(|model| model.model_id == spec.id)
            .expect("cached model projection");
        assert_eq!(orphan.status, "unverified_cache");
        assert!(orphan.verified_at_ms.is_none());
        assert!(observation.bounded(&next_boot.node_id));

        assert!(!accepted_model_identity(
            &next_boot.node_id,
            spec,
            replaced,
            Some(prior)
        ));
        next_boot.shutdown.cancel();
        drop(next_boot);

        let final_boot = persistent_state(root.path(), &database);
        let receipts = final_boot
            .store
            .subtitle_model_receipts(&final_boot.node_id)
            .await
            .expect("older receipt persists");
        assert!(!requalify_accepted_model(
            &final_boot,
            spec,
            replaced,
            changed_engine,
            receipts.first()
        )
        .await
        .expect("cancelled replacement cannot resurrect on another boot"));
        // Fresh explicit Install intentionally completes the orphan cache.
        model_receipt_fixture(&final_boot, replaced, ModelAction::Install, false).await;
        let receipts = final_boot
            .store
            .subtitle_model_receipts(&final_boot.node_id)
            .await
            .expect("replacement accepted");
        assert!(accepted_model_identity(
            &final_boot.node_id,
            spec,
            replaced,
            receipts.first()
        ));
        assert!(!accepted_model_identity(
            "another-node",
            spec,
            replaced,
            receipts.first()
        ));
        final_boot.shutdown.cancel();
    }

    #[test]
    fn managed_normalization_revision_is_bound_and_stale_recipes_are_not_executable() {
        let model = &catalog().models[0];
        let profile = plurx_core::store::background_jobs_transcription::ManagedProfile {
            model_id: model.id.clone(),
            catalog_revision: catalog().revision.clone(),
            adapter_contract: catalog().adapter_contract.clone(),
        };
        let mut payload = JobPayload::SubtitleTranscribeManaged {
            file_id: 1,
            audio_index: 7,
            source_size: 2,
            source_mtime: 3,
            language: "en".into(),
            model_sha256: model.sha256.clone(),
            pipeline_digest: managed_pipeline_digest(&profile, model, 7, "en"),
            profile: profile.clone(),
        };
        assert!(managed_recipe_current(&payload, model));
        let JobPayload::SubtitleTranscribeManaged {
            pipeline_digest, ..
        } = &mut payload
        else {
            unreachable!()
        };
        *pipeline_digest = hex::encode(Sha256::digest(format!(
            "{}\n{}\n{}\n{}\n{}\n",
            profile.adapter_contract, profile.catalog_revision, model.sha256, 7, "en"
        )));
        assert!(
            !managed_recipe_current(&payload, model),
            "older normalization recipe must not silently execute new output rules"
        );
    }
    #[tokio::test]
    async fn foreground_capacity_prevents_even_a_cold_model_open() {
        let root = crate::test_tempdir().expect("private root");
        let state = state(root.path());
        let admission = state
            .transcode
            .admit_fragment()
            .await
            .expect("idle admission");
        let demand = state.transcode.test_mark_live_waiting();
        let cancel = tokio_util::sync::CancellationToken::new();
        // No model store exists: any metadata/content access would fail with a
        // storage error. Capacity must win before that access or qualification.
        let error = hash_admitted_model(&state, &catalog().models[0], &cancel, &admission)
            .await
            .err()
            .expect("foreground refusal");
        assert!(error.to_string().contains("waiting_for_playback_capacity"));
        assert!(!state.subtitle_models.root.exists());
        assert!(!cancel.is_cancelled());
        drop(demand);
        assert!(state.transcode.fragment_worker_idle(&admission));
        state.shutdown.cancel();
    }
    #[tokio::test]
    async fn automatic_verification_retries_after_queue_capacity_returns_and_tracks_only_catalog_models(
    ) {
        let root = crate::test_tempdir().expect("private root");
        let state = state(root.path());
        let spec = &catalog().models[0];
        let now = crate::state::clock_ms();
        let mut ids = Vec::new();
        for index in 0..8 {
            let id = uuid::Uuid::new_v4().to_string();
            ids.push(id.clone());
            let outcome = state
                .store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::SubtitleModelOperation {
                        translation_profile: None,
                        action: ModelAction::Install,
                        model_id: format!("occupied-{index}"),
                        catalog_revision: catalog().revision.clone(),
                        model_sha256: spec.sha256.clone(),
                        adapter_contract: catalog().adapter_contract.clone(),
                        target_node_id: state.node_id.clone(),
                    },
                    dedupe_key: format!("full:{id}"),
                    priority: 0,
                    not_before_ms: now,
                    now_ms: now,
                    request: JobRequest {
                        scope: "subtitle-model".into(),
                        request_id: id.clone(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "subtitle_model_operation".into(),
                        consumer_ref: id,
                        target_node_id: Some(state.node_id.clone()),
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("fill queue");
            assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
        }
        assert!(
            !schedule_automatic_verification(&state, spec, "identity-one")
                .await
                .expect("bounded queue")
        );
        assert!(state
            .subtitle_models
            .automatic_verifications
            .read()
            .await
            .is_empty());
        for id in ids {
            state
                .store
                .cancel_job(CancelJob {
                    job_id: id,
                    now_ms: now + 1,
                })
                .await
                .expect("release capacity");
        }
        assert!(
            schedule_automatic_verification(&state, spec, "identity-one")
                .await
                .expect("retry after capacity")
        );
        assert!(
            schedule_automatic_verification(&state, spec, "identity-one")
                .await
                .expect("same identity coalesces")
        );
        assert!(
            schedule_automatic_verification(&state, spec, "identity-two")
                .await
                .expect("replaced inode has own verification demand")
        );
        assert_eq!(
            state
                .subtitle_models
                .automatic_verifications
                .read()
                .await
                .len(),
            1,
            "replacement does not grow per-file history"
        );
        let page = state
            .store
            .job_candidates(plurx_core::store::background_jobs::CandidateQuery {
                node_id: state.node_id.clone(),
                kinds: vec![plurx_core::store::background_jobs::JobKind::SubtitleModelOperation],
                after: None,
                now_ms: crate::state::clock_ms(),
                limit: 32,
            })
            .await
            .expect("candidates");
        assert_eq!(
            page.jobs.len(),
            2,
            "distinct local identities cannot coalesce with old running verification"
        );
        state.shutdown.cancel();
    }
    #[tokio::test]
    async fn inaccessible_source_worker_does_not_claim_another_workers_executable_job() {
        use plurx_core::domain::{
            AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
        };
        let root = crate::test_tempdir().expect("private root");
        let state = state(root.path());
        let path = root.path().join("source.mkv");
        std::fs::write(&path, b"source fixture").expect("source");
        let meta = std::fs::metadata(&path).expect("metadata");
        let mtime = meta
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Shared source".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
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
                title: "Shared source".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = state
            .store
            .upsert_file(
                item,
                path.to_str().expect("path"),
                meta.len() as i64,
                mtime,
                &ProbeResult {
                    duration_ms: Some(1000),
                    video_codec: Some("h264".into()),
                    audio_streams: vec![
                        AudioStream {
                            index: 0,
                            language: Some("en".into()),
                            ..Default::default()
                        },
                        AudioStream {
                            index: 1,
                            language: Some("en".into()),
                            ..Default::default()
                        },
                    ],
                    raw_json: Some("{}".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let file = state
            .store
            .get_file(file_id)
            .await
            .expect("file")
            .expect("file");
        let settings = config::read(state.store.as_ref()).await.expect("defaults");
        config::save(
            state.store.as_ref(),
            config::UpdateRequest {
                version: 1,
                expected_revision: settings.revision,
                mode: config::Mode::Managed,
                enabled: true,
                interval_mins: 0,
                language: "en".into(),
                model_id: catalog().models[0].id.clone(),
                advanced: None,
            },
            &catalog().models,
        )
        .await
        .expect("enable without model");
        let payload = managed_payload(&file, 0, "en", &catalog().models[0]);
        let key = plurx_core::store::background_jobs_transcription::artifact_key(&payload)
            .expect("artifact");
        let id = uuid::Uuid::new_v4().to_string();
        let now = crate::state::clock_ms();
        state
            .store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: payload.clone(),
                dedupe_key: key.clone(),
                priority: 0,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "subtitle-transcription".into(),
                    request_id: id.clone(),
                    request_digest: key.clone(),
                    consumer_kind: "subtitle_transcribe_managed".into(),
                    consumer_ref: key,
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue");
        let queued_payload = managed_payload(&file, 1, "en", &catalog().models[0]);
        let queued_id = uuid::Uuid::new_v4().to_string();
        let queued_key =
            plurx_core::store::background_jobs_transcription::artifact_key(&queued_payload)
                .expect("second artifact");
        state
            .store
            .enqueue_job(EnqueueJob {
                id: queued_id.clone(),
                payload: queued_payload.clone(),
                dedupe_key: queued_key.clone(),
                priority: 0,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "subtitle-transcription".into(),
                    request_id: queued_id.clone(),
                    request_digest: queued_key.clone(),
                    consumer_kind: "subtitle_transcribe_managed".into(),
                    consumer_ref: queued_key,
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("second queued request");
        let hidden = root.path().join("temporarily-unmounted");
        std::fs::rename(&path, &hidden).expect("simulate unavailable mount");
        assert!(matches!(
            prepare_generation_source(&state, &payload)
                .await
                .expect("eligibility"),
            SourceEligibility::LocalUnavailable
        ));
        assert_eq!(
            state
                .store
                .background_job(&id)
                .await
                .expect("job")
                .expect("job")
                .state,
            JobState::Queued
        );
        std::fs::rename(&hidden, &path).expect("second worker source available");
        let mut other = state.clone();
        other.node_id = "worker-two".into();
        let SourceEligibility::Ready(prepared) = prepare_generation_source(&other, &payload)
            .await
            .expect("second eligibility")
        else {
            panic!("accessible source");
        };
        assert!(prepared.held.unchanged());
        let result = state
            .store
            .claim_artifact_job(ClaimJob {
                job_id: id,
                expected_revision: 0,
                node_id: other.node_id.clone(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: payload.kind(),
                payload_version: 1,
                now_ms: now + 1,
                dispatched_at_ms: now + 1,
            })
            .await
            .expect("second worker claim");
        assert!(
            matches!(result,ClaimOutcome::Claimed{job} if job.token.as_ref().expect("token").node_id=="worker-two")
        );
        state
            .store
            .upsert_file(
                item,
                path.to_str().expect("path"),
                file.size + 1,
                mtime,
                &ProbeResult {
                    duration_ms: Some(1000),
                    video_codec: Some("h264".into()),
                    audio_streams: file.audio_streams.clone(),
                    raw_json: Some("{}".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("authoritative source revision changes");
        assert!(matches!(
            prepare_generation_source(&state, &queued_payload)
                .await
                .expect("obsolete eligibility"),
            SourceEligibility::Obsolete
        ));
        assert_eq!(
            state
                .store
                .background_job(&queued_id)
                .await
                .expect("job")
                .expect("queued job")
                .state,
            JobState::Cancelled,
            "authoritatively obsolete work does not remain waiting for a mount"
        );
        state.shutdown.cancel();
    }
    async fn test_media(
        state: &AppState,
        label: &str,
        language: &str,
        caption: Option<&str>,
    ) -> plurx_core::domain::MediaFile {
        use plurx_core::domain::{
            AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult, SubtitleStream,
        };
        let library = state
            .store
            .create_library(&NewLibrary {
                name: label.into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/media")],
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
                title: label.into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let id = state
            .store
            .upsert_file(
                item,
                &format!("/media/{label}.mkv"),
                100,
                100,
                &ProbeResult {
                    duration_ms: Some(1000),
                    video_codec: Some("h264".into()),
                    audio_streams: vec![AudioStream {
                        index: 0,
                        language: Some(language.into()),
                        default: true,
                        ..Default::default()
                    }],
                    subtitle_streams: caption
                        .map(|language| {
                            vec![SubtitleStream {
                                index: 0,
                                language: Some(language.into()),
                                ..Default::default()
                            }]
                        })
                        .unwrap_or_default(),
                    raw_json: Some("{}".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        state
            .store
            .get_file(id)
            .await
            .expect("get file")
            .expect("file")
    }
    async fn test_settings(
        state: &AppState,
        enabled: bool,
        interval: u32,
        model: &str,
    ) -> config::Snapshot {
        let current = config::read(state.store.as_ref())
            .await
            .expect("current config");
        config::save(
            state.store.as_ref(),
            config::UpdateRequest {
                version: 1,
                expected_revision: current.revision,
                mode: config::Mode::Managed,
                enabled,
                interval_mins: interval,
                language: "en".into(),
                model_id: model.into(),
                advanced: None,
            },
            &catalog().models,
        )
        .await
        .expect("save advisory choice")
    }
    #[tokio::test]
    async fn bounded_candidate_cursor_reaches_install_after_thirty_two_ineligible_generation_jobs()
    {
        use plurx_core::store::background_jobs::JobKind;
        let root = crate::test_tempdir().expect("root");
        let state = state(root.path());
        let file = test_media(&state, "candidate-pagination", "en", None).await;
        test_settings(&state, true, 0, "base.en").await;
        for index in 0..32 {
            let mut request = generation_enqueue(&file, index, "en", &catalog().models[0])
                .expect("enqueue input");
            request.priority = 1;
            let JobPayload::SubtitleTranscribeManaged { profile, .. } = &mut request.payload else {
                panic!("managed")
            };
            profile.model_id = "not-installed-catalog-model".into();
            assert!(matches!(
                state.store.enqueue_job(request).await.expect("queued"),
                EnqueueOutcome::Accepted { .. }
            ));
        }
        let operation = enqueue_model_operation(
            &state,
            &catalog().models[0],
            &state.node_id,
            ModelAction::Install,
            None,
        )
        .await
        .expect("install");
        let EnqueueOutcome::Accepted { job_id, .. } = operation else {
            panic!("accepted install")
        };
        let kinds = vec![
            JobKind::SubtitleTranscribeManaged,
            JobKind::SubtitleModelOperation,
        ];
        let first = next_candidates(&state, kinds.clone())
            .await
            .expect("first page");
        assert_eq!(first.jobs.len(), 32);
        assert!(first
            .jobs
            .iter()
            .all(|job| job.supported_payload().expect("payload").kind()
                == JobKind::SubtitleTranscribeManaged));
        let second = next_candidates(&state, kinds.clone())
            .await
            .expect("next bounded pass");
        let job = second
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("later eligible install");
        let now = crate::state::clock_ms();
        assert!(matches!(
            state
                .store
                .claim_artifact_job(ClaimJob {
                    job_id: job.id.clone(),
                    expected_revision: job.revision,
                    node_id: state.node_id.clone(),
                    boot_id: state.subtitle_models.boot_id.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::SubtitleModelOperation,
                    payload_version: 1,
                    now_ms: now,
                    dispatched_at_ms: now
                })
                .await
                .expect("claim later work"),
            ClaimOutcome::Claimed { .. }
        ));
        assert!(
            state
                .subtitle_models
                .candidate_cursor
                .read()
                .await
                .is_none(),
            "end wraps to permit earlier newly eligible work"
        );
        assert_eq!(
            next_candidates(&state, kinds)
                .await
                .expect("wrapped pass")
                .jobs
                .len(),
            32
        );
        state.shutdown.cancel();
    }
    #[tokio::test]
    async fn managed_discovery_obeys_known_language_cursor_interval_and_configuration_revision() {
        use plurx_core::store::background_jobs::JobKind;
        let root = crate::test_tempdir().expect("root");
        let state = state(root.path());
        for index in 0..10 {
            test_media(&state, &format!("sweep-{index}"), "eng", None).await;
        }
        test_media(&state, "sweep-unknown", " UND ", None).await;
        test_media(&state, "sweep-covered", "en", Some("en")).await;
        test_media(&state, "sweep-other-caption", "en", Some("fr")).await;
        let settings = test_settings(&state, true, 60, "base.en").await;
        let lease = state
            .jobs
            .acquire_job("subtitle-transcription:sweep".into())
            .await
            .expect("lease")
            .expect("lease");
        automatic_page(&state, &settings, &lease)
            .await
            .expect("first page");
        let raw = state
            .store
            .get_setting(MANAGED_SCHEDULE_KEY)
            .await
            .expect("schedule")
            .expect("schedule");
        let mut schedule: Schedule = serde_json::from_str(&raw).expect("bounded schedule");
        assert_eq!(schedule.cursor, 8);
        let count = || plurx_core::store::background_jobs::CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::SubtitleTranscribeManaged],
            after: None,
            now_ms: crate::state::clock_ms(),
            limit: 32,
        };
        assert_eq!(
            state
                .store
                .job_candidates(count())
                .await
                .expect("jobs")
                .jobs
                .len(),
            8
        );
        automatic_page(&state, &settings, &lease)
            .await
            .expect("interval defers scan");
        assert_eq!(
            state
                .store
                .job_candidates(count())
                .await
                .expect("jobs")
                .jobs
                .len(),
            8
        );
        schedule.next_run_ms = 0;
        state
            .store
            .put_settings(&[(MANAGED_SCHEDULE_KEY, &schedule.encoded().expect("encoding"))])
            .await
            .expect("clock due");
        automatic_page(&state, &settings, &lease)
            .await
            .expect("next page");
        assert_eq!(
            state
                .store
                .job_candidates(count())
                .await
                .expect("jobs")
                .jobs
                .len(),
            11,
            "unknown spoken language and existing English excluded, French caption permits English"
        );
        let changed = test_settings(&state, true, 60, "base").await;
        automatic_page(&state, &changed, &lease)
            .await
            .expect("new model ignores stale future pacing");
        assert_eq!(
            state
                .store
                .job_candidates(count())
                .await
                .expect("jobs")
                .jobs
                .len(),
            19,
            "new model profile queued without rewriting accepted old requests"
        );
        let stopped = test_settings(&state, true, 0, "base.en").await;
        automatic_page(&state, &stopped, &lease)
            .await
            .expect("manual only");
        let disabled = test_settings(&state, false, 60, "base.en").await;
        automatic_page(&state, &disabled, &lease)
            .await
            .expect("disabled");
        assert_eq!(
            state
                .store
                .job_candidates(count())
                .await
                .expect("jobs")
                .jobs
                .len(),
            19
        );
        let replay = Schedule::current(
            Some(&schedule.encoded().expect("old schedule")),
            &changed.revision,
        );
        assert_eq!(replay.next_run_ms, 0);
        assert_eq!(replay.cursor, 0);
        lease.release().await.expect("release");
        state.shutdown.cancel();
    }
}
