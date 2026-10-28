//! Closed public translation projections. Store recipes, paths and source
//! binding authority never cross this boundary.
use super::config::{self, UpdateRequest};
use crate::{
    http::{
        error::ApiError,
        extract::{AdminUser, AuthUser},
    },
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Path as ApiPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use plurx_core::{
    domain::MediaFile,
    store::{
        background_jobs::{BackgroundJob, JobPayload, JobState},
        subtitle_translation::{
            digest, valid_digest, SourceRevision, SpeechTask, TranslationProfile,
        },
        subtitle_translation_mutation::{TranslationAction, TranslationMutation},
        subtitle_translation_read::{AcquireTranslationLease, TranslationQuery, TranslationRead},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PAGE_BYTES: usize = 65_536;
const LEASE_HEADER: &str = "x-translation-read-lease";
const CLOCK_HEADER: &str = "x-translation-server-now-ms";
const EXPIRY_HEADER: &str = "x-translation-read-lease-expires-at-ms";

fn invalid() -> ApiError {
    config::failure(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        crate::state::clock_ms(),
    )
}
fn observation_error() -> ApiError {
    ApiError::Internal("Translation observation has incompatible fields".into())
}
fn id(value: &str) -> Result<(), ApiError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| invalid())
}
fn number(value: &str, positive: bool) -> Result<i64, ApiError> {
    let parsed = value.parse::<i64>().map_err(|_| invalid())?;
    if parsed < i64::from(positive) || parsed.to_string() != value {
        return Err(invalid());
    }
    Ok(parsed)
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, ApiError> {
    value[name].as_str().ok_or_else(observation_error)
}
fn counter(value: &Value, name: &str, positive: bool) -> Result<String, ApiError> {
    let value = field(value, name)?;
    number(value, positive).map_err(|_| observation_error())?;
    Ok(value.into())
}
fn ms(value: i64) -> Result<i64, ApiError> {
    if !(0..=14_400_000).contains(&value) {
        return Err(invalid());
    }
    Ok(value)
}
fn envelope(version: u8, request_id: &str, expires: i64) -> Result<(), ApiError> {
    if version != 1 || expires < 0 {
        return Err(invalid());
    }
    id(request_id)
}
fn response(value: impl Serialize) -> Result<Response, ApiError> {
    let bytes = serde_json::to_vec(&value).map_err(|_| observation_error())?;
    if bytes.len() > PAGE_BYTES {
        return Err(observation_error());
    }
    let mut answer = Response::new(Body::from(bytes));
    answer.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    private_headers(&mut answer, crate::state::clock_ms(), None)?;
    Ok(answer)
}
fn private_headers(response: &mut Response, now: i64, expiry: Option<i64>) -> Result<(), ApiError> {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private,no-cache"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
    headers.insert(
        CLOCK_HEADER,
        HeaderValue::from_str(&now.to_string()).map_err(|_| observation_error())?,
    );
    if let Some(expiry) = expiry {
        headers.insert(
            EXPIRY_HEADER,
            HeaderValue::from_str(&expiry.to_string()).map_err(|_| observation_error())?,
        );
    }
    Ok(())
}
fn languages() -> Vec<String> {
    super::profile::model_spec_for(&super::profile::default_profile())
        .expect("registered default speech profile")
        .languages
}
fn selected_profile(model: &str, profile_id: &str) -> Result<TranslationProfile, ApiError> {
    super::profile::profiles()
        .into_iter()
        .find(|p| model == p.model_id && p.id().is_ok_and(|id| id == profile_id))
        .ok_or_else(invalid)
}
/// Saved preferences choose metadata defaults only; readiness never changes them.
fn configured_default_profile(saved: &config::Snapshot) -> Option<TranslationProfile> {
    if let Some(id) = saved.profile_id.as_deref() {
        return super::profile::profiles().into_iter().find(|profile| {
            profile.id().is_ok_and(|candidate| candidate == id)
                && saved
                    .model_id
                    .as_deref()
                    .is_none_or(|model| model == profile.model_id)
        });
    }
    if let Some(model) = saved.model_id.as_deref() {
        return super::profile::profiles()
            .into_iter()
            .find(|profile| profile.model_id == model && profile.task.is_translation());
    }
    Some(super::profile::default_profile())
}
fn profile_summary(
    profile: TranslationProfile,
    preferred_model: Option<&str>,
) -> Result<Value, ApiError> {
    let model = super::profile::model_spec_for(&profile)?;
    let is_default = preferred_model == Some(profile.model_id.as_str());
    Ok(
        json!({"profile_id":profile.id()?,"model_id":profile.model_id,"model_sha256":profile.model_sha256,
        "label":model.label,"default":is_default,"task":profile.task.name(),"output_language":if profile.task.is_translation(){Some("en")}else{None},"input_languages":model.languages.into_iter().filter(|language| !profile.task.is_translation() || language != "en").collect::<Vec<_>>(),"adapter_contract":profile.adapter_contract,
        "pipeline_revision":profile.pipeline_revision,"timeline_contract":profile.timeline_contract,
        "window":{"core_ms":profile.core_ms,"left_context_ms":profile.left_context_ms,"right_context_ms":profile.right_context_ms},
        "eligibility":{"delivery":["direct","remux","transcode"],"max_source_audio_streams":256,"clock_contract":profile.timeline_contract},
        "limits":{"segment_bytes":16384,"segment_cues":128,"page_bytes":65536,"page_entries":64,"coverage_intervals":64,
            "concurrent_fetches":2,"cue_memory_bytes":524288,"cue_count":1024,"cue_window_before_ms":30000,"cue_window_after_ms":90000,
            "poll_min_ms":1000,"poll_max_ms":5000,"retry_max_ms":30000,"heartbeat_interval_ms":5000,"demand_ttl_ms":15000,"max_following_cores":3},
        "activation_state":"active","activation_reason":"experimental_limited_profile"}),
    )
}
pub async fn settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    response(config::read(state.store.as_ref(), crate::state::clock_ms()).await?)
}
pub async fn update_settings(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(request): Json<UpdateRequest>,
) -> Result<Response, ApiError> {
    response(
        config::save(
            state.store.as_ref(),
            user.id,
            request,
            &languages(),
            crate::state::clock_ms(),
        )
        .await?,
    )
}
pub async fn models(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let preferred = configured_default_profile(
        &config::read(state.store.as_ref(), crate::state::clock_ms()).await?,
    );
    let default_profile_id = preferred.as_ref().map(TranslationProfile::id).transpose()?;
    let preferred_model = preferred.as_ref().map(|profile| profile.model_id.as_str());
    let models = super::profile::model_specs().into_iter().map(|model| json!({"id":model.id,"label":model.label,"download_bytes":model.download_bytes,"sha256":model.sha256,"source":model.source,"license":model.license})).collect::<Vec<_>>();
    response(
        json!({"version":1,"catalog_revision":"plurx-managed-subtitle-models-v2",
        "models":models,"default_profile_id":default_profile_id,
        "profiles":super::profile::profiles().into_iter().map(|profile| profile_summary(profile, preferred_model)).collect::<Result<Vec<_>,_>>()?,"workers":worker_rows(super::workers_public(&state).await?)?,"server_now_ms":crate::state::clock_ms()}),
    )
}
fn worker_rows(rows: Vec<Value>) -> Result<Vec<Value>, ApiError> {
    if rows.len() > 32 {
        return Err(observation_error());
    }
    rows.iter().map(|row| {
        let node = field(row, "node_id")?;
        let label = field(row, "label")?;
        if node.is_empty() || node.len() > 128 || label.len() > 256 { return Err(observation_error()); }
        let engine = if row["engine"].is_null() { Value::Null } else {
            let hash = field(&row["engine"], "binary_sha256")?;
            if !valid_digest(hash) { return Err(observation_error()); }
            json!({"adapter_contract":field(&row["engine"],"adapter_contract")?,"binary_sha256":hash,"platform":field(&row["engine"],"platform")?})
        };
        let capabilities = row["capabilities"].as_array().filter(|rows| rows.len() <= 16).ok_or_else(observation_error)?;
        let capabilities = capabilities.iter().map(|cap| {
            let status = field(cap, "status")?;
            if !matches!(status, "missing" | "installed" | "verified" | "verification_failed" | "unknown" | "unverified_cache") { return Err(observation_error()); }
            let model_sha = field(cap, "model_sha256")?;
            let profile_id = field(cap, "profile_id")?;
            if !valid_digest(model_sha) || !valid_digest(profile_id) { return Err(observation_error()); }
            let engine_sha = nullable_digest(cap, "engine_sha256")?;
            let verified = if cap["verified_at_ms"].is_null() { Value::Null } else { cap["verified_at_ms"].as_i64().filter(|n| *n>=0).ok_or_else(observation_error)?.into() };
            Ok(json!({"task":cap["task"],"model_id":field(cap,"model_id")?,"profile_id":profile_id,"status":status,"reason":safe_reason(cap["reason"].as_str()),"verified_at_ms":verified,"model_sha256":model_sha,"engine_sha256":engine_sha}))
        }).collect::<Result<Vec<_>,ApiError>>()?;
        Ok(json!({"node_id":node,"label":label,"current":row["current"].as_bool().ok_or_else(observation_error)?,"eligible":row["eligible"].as_bool().ok_or_else(observation_error)?,"observed_at_ms":row["observed_at_ms"].as_i64().ok_or_else(observation_error)?,"expires_at_ms":row["expires_at_ms"].as_i64().ok_or_else(observation_error)?,"engine":engine,"capacity":row["capacity"],"capabilities":capabilities}))
    }).collect()
}
async fn file(state: &AppState, file_id: i64) -> Result<MediaFile, ApiError> {
    if file_id <= 0 {
        return Err(invalid());
    }
    state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))
}
async fn read_rows(
    state: &AppState,
    user_id: i64,
    query: TranslationQuery,
) -> Result<Vec<Value>, ApiError> {
    // HTTP never uses the worker-only user zero bypass.
    if user_id <= 0 {
        return Err(ApiError::Unauthorized);
    }
    Ok(state
        .store
        .read_subtitle_translation(TranslationRead {
            user_id,
            now_ms: crate::state::clock_ms(),
            query,
        })
        .await?)
}
async fn operation_row(
    state: &AppState,
    user_id: i64,
    operation_id: &str,
) -> Result<Value, ApiError> {
    id(operation_id)?;
    let mut rows = read_rows(
        state,
        user_id,
        TranslationQuery::Operation {
            operation_id: operation_id.into(),
        },
    )
    .await?;
    let row = rows
        .pop()
        .ok_or(ApiError::NotFound("translation operation"))?;
    let current = file(state, number(field(&row, "file_id")?, true)?).await?;
    if current.size.to_string() != field(&row, "source_size")?
        || current.mtime.to_string() != field(&row, "source_mtime")?
    {
        return Err(config::failure(
            StatusCode::CONFLICT,
            "source_changed",
            crate::state::clock_ms(),
        ));
    }
    Ok(row)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetSummary {
    asset_id: String,
    file_id: String,
    source_size: String,
    source_mtime: String,
    content_revision: String,
    profile_id: String,
    audio_index: i64,
    spoken_language: String,
    output_language: String,
    #[serde(default)]
    task: SpeechTask,
    timeline_contract: String,
    manifest_revision: String,
    duration_ms: i64,
    core_count: i64,
    operation_id: String,
}
fn asset_summary(value: Value) -> Result<AssetSummary, ApiError> {
    let row: AssetSummary = serde_json::from_value(value).map_err(|_| observation_error())?;
    if !valid_digest(&row.asset_id)
        || !valid_digest(&row.content_revision)
        || !valid_digest(&row.profile_id)
        || row.output_language != row.task.output_language(&row.spoken_language)
        || !(0..=255).contains(&row.audio_index)
    {
        return Err(observation_error());
    }
    id(&row.operation_id).map_err(|_| observation_error())?;
    number(&row.file_id, true).map_err(|_| observation_error())?;
    number(&row.source_size, true).map_err(|_| observation_error())?;
    if row
        .source_mtime
        .parse::<i64>()
        .map(|n| n.to_string())
        .ok()
        .as_deref()
        != Some(&row.source_mtime)
    {
        return Err(observation_error());
    }
    number(&row.manifest_revision, false).map_err(|_| observation_error())?;
    ms(row.duration_ms).map_err(|_| observation_error())?;
    Ok(row)
}
pub async fn options(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(file_id): ApiPath<i64>,
) -> Result<Response, ApiError> {
    let preferred = configured_default_profile(
        &config::read(state.store.as_ref(), crate::state::clock_ms()).await?,
    );
    let default_profile_id = preferred.as_ref().map(TranslationProfile::id).transpose()?;
    let preferred_model = preferred.as_ref().map(|profile| profile.model_id.as_str());
    let file = file(&state, file_id).await?;
    let (assets, next) = asset_page(&state, user.id, file.id, None).await?;
    let mut audio = Vec::new();
    let mut any_supported = false;
    let mut source_origin_ms = None;
    let mut capability_reason = "direct_audio_selection_unverified";
    for track in &file.audio_streams {
        let clock = super::selected_file_capability(&state, &file, track.index).await;
        if let Ok(clock) = clock {
            any_supported = true;
            source_origin_ms = Some(clock.origin_samples.div_euclid(16));
        }
        if let Err(reason) = clock {
            capability_reason = reason;
        }
        audio.push(json!({"index":track.index,"audio_ordinal":track.index,"source_index":clock.as_ref().ok().and_then(|clock|clock.source_index),"source_start_ms":clock.as_ref().ok().map(|clock|clock.audio_start_samples.div_euclid(16)),"duration_ms":clock.as_ref().ok().map(|clock|clock.duration_ms),"capability":{"supported":clock.is_ok(),"reason":clock.err()},"language":track.language,"normalized_language":normalized_language(track.language.as_deref()),"title":track.title,"default":track.default}));
    }
    response(
        json!({"version":1,"file_id":file.id.to_string(),"source_size":file.size.to_string(),"source_mtime":file.mtime.to_string(),
        "content_revision":null,"duration_ms":file.duration_ms,"source_origin_ms":source_origin_ms,"timeline_contract":super::profile::selected_profile().timeline_contract,
        "default_profile_id":default_profile_id,"audio_tracks":audio,"subtitle_streams":file.subtitle_streams,"models":super::profile::profiles().into_iter().map(|profile| profile_summary(profile, preferred_model)).collect::<Result<Vec<_>,_>>()?,
        "capability":{"supported":any_supported,"reason":if any_supported{None}else{Some(capability_reason)}},
        "permissions":{"read":true,"demand":user.is_admin,"stop":user.is_admin,"resume":user.is_admin},
        "assets":assets,"assets_next_cursor":next,"server_now_ms":crate::state::clock_ms()}),
    )
}
fn normalized_language(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() || matches!(raw.to_ascii_lowercase().as_str(), "und" | "unknown" | "n/a") {
        return None;
    }
    let value = crate::subtitle_transcription::normalized_language(raw);
    plurx_core::store::background_jobs_transcription::whisper_language(&value).then_some(value)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    kind: String,
    user_id: String,
    file_id: Option<String>,
    asset_id: Option<String>,
    operation_id: Option<String>,
    lease_id: Option<String>,
    snapshot_revision: Option<String>,
    epoch: Option<String>,
    view_revision: Option<String>,
    after: String,
    through: Option<i64>,
    window_start_ms: Option<i64>,
    window_end_ms: Option<i64>,
}
fn encode_cursor(cursor: &Cursor) -> Result<String, ApiError> {
    let encoded =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).map_err(|_| observation_error())?);
    if encoded.len() > 1024 {
        return Err(observation_error());
    }
    Ok(encoded)
}
fn decode_cursor(raw: &str, kind: &str, user_id: i64) -> Result<Cursor, ApiError> {
    if raw.len() > 1024 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if cursor.version != 1
        || cursor.kind != kind
        || cursor.user_id != user_id.to_string()
        || encode_cursor(&cursor)? != raw
    {
        return Err(invalid());
    }
    Ok(cursor)
}
fn cursor(kind: &str, user_id: i64) -> Cursor {
    Cursor {
        version: 1,
        kind: kind.into(),
        user_id: user_id.to_string(),
        file_id: None,
        asset_id: None,
        operation_id: None,
        lease_id: None,
        snapshot_revision: None,
        epoch: None,
        view_revision: None,
        after: String::new(),
        through: None,
        window_start_ms: None,
        window_end_ms: None,
    }
}
async fn asset_page(
    state: &AppState,
    user_id: i64,
    file_id: i64,
    after: Option<String>,
) -> Result<(Vec<AssetSummary>, Option<String>), ApiError> {
    let rows = read_rows(state, user_id, TranslationQuery::Assets { file_id, after }).await?;
    if rows.len() > 17 {
        return Err(observation_error());
    }
    let more = rows.len() > 16;
    let entries = rows
        .into_iter()
        .take(16)
        .map(asset_summary)
        .collect::<Result<Vec<_>, _>>()?;
    let next = if more {
        let mut c = cursor("assets", user_id);
        c.file_id = Some(file_id.to_string());
        c.after = entries
            .last()
            .ok_or_else(observation_error)?
            .asset_id
            .clone();
        Some(encode_cursor(&c)?)
    } else {
        None
    };
    Ok((entries, next))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageQuery {
    cursor: String,
}
pub async fn assets(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(file_id): ApiPath<i64>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    file(&state, file_id).await?;
    let c = decode_cursor(&query.cursor, "assets", user.id)?;
    if c.file_id.as_deref() != Some(&file_id.to_string()) || !valid_digest(&c.after) {
        return Err(invalid());
    }
    let (entries, next_cursor) = asset_page(&state, user.id, file_id, Some(c.after)).await?;
    response(
        json!({"version":1,"file_id":file_id.to_string(),"entries":entries,"next_cursor":next_cursor,"server_now_ms":crate::state::clock_ms()}),
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    version: u8,
    request_id: String,
    intent_expires_at_ms: i64,
    source_size: String,
    source_mtime: String,
    audio_index: i64,
    spoken_language: String,
    output_language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<SpeechTask>,
    model_id: String,
    profile_id: String,
    playback_id: String,
    position_ms: i64,
    playback_rate: f64,
    paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuity_id: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DemandRequest {
    version: u8,
    request_id: String,
    intent_expires_at_ms: i64,
    expected_operation_epoch: String,
    demand_generation: String,
    playback_id: String,
    position_ms: i64,
    playback_rate: f64,
    paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuity_id: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseRequest {
    version: u8,
    request_id: String,
    intent_expires_at_ms: i64,
    expected_operation_epoch: String,
    demand_generation: String,
    playback_id: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopRequest {
    version: u8,
    request_id: String,
    intent_expires_at_ms: i64,
    expected_operation_epoch: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RetryRequest {
    Operation {
        version: u8,
        request_id: String,
        intent_expires_at_ms: i64,
        expected_operation_epoch: String,
        expected_view_revision: String,
        expected_state: String,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
    Range {
        version: u8,
        request_id: String,
        intent_expires_at_ms: i64,
        expected_operation_epoch: String,
        expected_operation_state: String,
        core_index: i64,
        expected_range_revision: String,
        expected_range_state: String,
        playback_id: String,
        position_ms: i64,
        playback_rate: f64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuity_id: Option<String>,
    },
}
fn window(playback_id: &str, position: i64, rate: f64) -> Result<(), ApiError> {
    id(playback_id)?;
    ms(position)?;
    if !rate.is_finite() || rate <= 0.0 || rate > 4.0 {
        return Err(invalid());
    }
    Ok(())
}
fn mutation_error(outcome: &str, now: i64) -> ApiError {
    let (status, code) = match outcome {
        "forbidden" => (StatusCode::FORBIDDEN, "permission_denied"),
        "intent_expired" => (StatusCode::GONE, "intent_expired"),
        "request_conflict" => (StatusCode::CONFLICT, "request_conflict"),
        "source_changed" => (StatusCode::CONFLICT, "source_changed"),
        "disabled" => (StatusCode::CONFLICT, "disabled"),
        "stopped" | "already_stopped" => (StatusCode::CONFLICT, "operation_stopped"),
        "demand_conflict" => (StatusCode::CONFLICT, "demand_conflict"),
        "state_conflict" | "range_conflict" | "counter_exhausted" => {
            (StatusCode::CONFLICT, "epoch_conflict")
        }
        "receipt_quota" | "rate_limited" | "demand_quota" | "operation_quota" => {
            (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded")
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, "network_error"),
    };
    config::failure(status, code, now)
}
async fn mutate(
    state: &AppState,
    user_id: i64,
    request_id: String,
    request_digest: String,
    expires: i64,
    action: TranslationAction,
    wire_outcome: &str,
) -> Result<Response, ApiError> {
    let continuity = match &action {
        TranslationAction::Create { continuity_id, .. }
        | TranslationAction::Demand { continuity_id, .. }
        | TranslationAction::RetryOperation { continuity_id, .. }
        | TranslationAction::RetryRange { continuity_id, .. } => continuity_id.as_deref(),
        _ => None,
    };
    if continuity.is_some_and(|id| id.len() != 36 || uuid::Uuid::parse_str(id).is_err()) {
        return Err(invalid());
    }
    let now = crate::state::clock_ms();
    if let Some(receipt) = config::replay(
        state.store.as_ref(),
        user_id,
        &request_id,
        &request_digest,
        now,
    )
    .await?
    {
        return response(mutation_receipt(&receipt, wire_outcome)?);
    }
    config::fresh_intent(expires, now)?;
    let reconcile_producer = matches!(
        &action,
        TranslationAction::Create { .. } | TranslationAction::Demand { .. }
    );
    let value = state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id,
            request_id,
            request_digest,
            intent_expires_at_ms: expires,
            now_ms: now,
            action,
        })
        .await?;
    match value["outcome"].as_str() {
        Some("accepted") => {}
        Some(outcome) => return Err(mutation_error(outcome, now)),
        None => return Err(observation_error()),
    }
    // Exact receipt replay returned above. Producer recovery is best effort and
    // must never turn an accepted demand renewal into an ambiguous failure.
    if reconcile_producer {
        if let Some(operation_id) = value["operation_id"].as_str() {
            let recovery = tokio::time::timeout(
                std::time::Duration::from_secs(4),
                super::manager::reconcile_producer(state, operation_id, user_id),
            )
            .await;
            if !matches!(recovery, Ok(Ok(()))) {
                tracing::debug!("caption producer reconciliation deferred");
            }
        }
    }
    // Create, demand, Stop and Retry change what the worker should do now.
    state.subtitle_models.translation.wake.notify_one();
    response(mutation_receipt(&value, wire_outcome)?)
}
fn nullable_digest(value: &Value, key: &str) -> Result<Value, ApiError> {
    match &value[key] {
        Value::Null => Ok(Value::Null),
        Value::String(id) if valid_digest(id) => Ok(id.clone().into()),
        _ => Err(observation_error()),
    }
}
fn nullable_id(value: &Value, key: &str) -> Result<Value, ApiError> {
    match &value[key] {
        Value::Null => Ok(Value::Null),
        Value::String(value) => {
            id_check(value)?;
            Ok(value.clone().into())
        }
        _ => Err(observation_error()),
    }
}
fn mutation_receipt(value: &Value, outcome: &str) -> Result<Value, ApiError> {
    let operation_id = field(value, "operation_id")?;
    id_check(operation_id)?;
    let mut answer = json!({"version":1,"outcome":outcome,"operation_id":operation_id,
        "operation_epoch":counter(value,"operation_epoch",true)?,"view_revision":counter(value,"view_revision",true)?,
        "server_now_ms":value["server_now_ms"].as_i64().ok_or_else(observation_error)?});
    if matches!(outcome, "accepted" | "resumed" | "stopped") {
        let state = field(value, "state")?;
        operation_state(state)?;
        answer["state"] = state.into();
        answer["asset_id"] = nullable_digest(value, "asset_id")?;
    }
    if outcome == "accepted" {
        answer["job_id"] = nullable_id(value, "job_id")?;
    }
    if matches!(
        outcome,
        "accepted" | "resumed" | "updated" | "released" | "range_retry_queued"
    ) {
        let demand = field(value, "demand_id")?;
        id_check(demand)?;
        answer["demand_id"] = demand.into();
        answer["continuity_id"] = nullable_id(value, "continuity_id")?;
        answer["demand_generation"] = counter(value, "demand_generation", true)?.into();
    }
    if outcome == "updated" {
        answer["expires_at_ms"] = value["expires_at_ms"]
            .as_i64()
            .ok_or_else(observation_error)?
            .into();
    }
    if outcome == "range_retry_queued" {
        answer["core_index"] = value["core_index"]
            .as_i64()
            .filter(|n| (0..2048).contains(n))
            .ok_or_else(observation_error)?
            .into();
        answer["range_revision"] = counter(value, "range_revision", true)?.into();
        let job = field(value, "job_id")?;
        id_check(job)?;
        answer["job_id"] = job.into();
    }
    Ok(answer)
}

pub async fn create(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    ApiPath(file_id): ApiPath<i64>,
    Json(input): Json<CreateRequest>,
) -> Result<Response, ApiError> {
    envelope(input.version, &input.request_id, input.intent_expires_at_ms)?;
    window(&input.playback_id, input.position_ms, input.playback_rate)?;
    let task = input.task.unwrap_or_else(|| {
        super::profile::profiles()
            .into_iter()
            .find(|profile| profile.id().is_ok_and(|id| id == input.profile_id))
            .map_or(SpeechTask::default(), |profile| profile.task)
    });
    if input.output_language != task.output_language(&input.spoken_language)
        || !(0..=255).contains(&input.audio_index)
        || !plurx_core::store::background_jobs_transcription::whisper_language(
            &input.spoken_language,
        )
        || (task.is_translation() && input.spoken_language == "en")
        || file_id <= 0
        || !valid_digest(&input.profile_id)
        || input.model_id.is_empty()
        || input.model_id.len() > 64
    {
        return Err(invalid());
    }
    number(&input.source_size, false)?;
    if input
        .source_mtime
        .parse::<i64>()
        .ok()
        .map(|n| n.to_string())
        .as_deref()
        != Some(input.source_mtime.as_str())
    {
        return Err(invalid());
    }
    let request_digest = config::request_digest(
        "POST",
        &format!("/files/{file_id}/subtitle-translation"),
        &input,
    )?;
    let now = crate::state::clock_ms();
    if let Some(receipt) = config::replay(
        state.store.as_ref(),
        user.id,
        &input.request_id,
        &request_digest,
        now,
    )
    .await?
    {
        return response(mutation_receipt(&receipt, "accepted")?);
    }
    config::fresh_intent(input.intent_expires_at_ms, now)?;
    let profile = selected_profile(&input.model_id, &input.profile_id)?;
    if profile.task != task {
        return Err(invalid());
    }
    if !languages().contains(&input.spoken_language) {
        return Err(invalid());
    }
    let file = file(&state, file_id).await?;
    if input.source_size != file.size.to_string() || input.source_mtime != file.mtime.to_string() {
        return Err(config::failure(
            StatusCode::CONFLICT,
            "source_changed",
            crate::state::clock_ms(),
        ));
    }
    let duration = super::selected_file_capability(&state, &file, input.audio_index)
        .await
        .map_err(|code| config::failure(StatusCode::CONFLICT, code, crate::state::clock_ms()))?;
    let recipe_key = digest(
        &json!({"source":SourceRevision { file_id:file.id,source_size:file.size,source_mtime:file.mtime },
        "profile":profile,"audio_index":input.audio_index,"spoken_language":input.spoken_language,"duration_ms":duration.duration_ms}),
    )?;
    let existing = read_rows(
        &state,
        user.id,
        TranslationQuery::RecipeOperation { recipe_key },
    )
    .await?;
    let target = if let Some(existing) = existing.first() {
        field(existing, "target_node_id")?.to_owned()
    } else {
        super::eligible_worker(&state, &file, &profile).await?
    };
    mutate(
        &state,
        user.id,
        input.request_id,
        request_digest,
        input.intent_expires_at_ms,
        TranslationAction::Create {
            source: SourceRevision {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
            },
            profile,
            audio_index: input.audio_index,
            spoken_language: input.spoken_language,
            duration_ms: duration.duration_ms,
            target_node_id: target,
            playback_id: input.playback_id,
            position_ms: input.position_ms,
            playback_rate: input.playback_rate,
            paused: input.paused,
            continuity_id: input.continuity_id,
        },
        "accepted",
    )
    .await
}
pub async fn demand(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    ApiPath((operation_id, demand_id)): ApiPath<(String, String)>,
    Json(input): Json<DemandRequest>,
) -> Result<Response, ApiError> {
    envelope(input.version, &input.request_id, input.intent_expires_at_ms)?;
    id(&operation_id)?;
    id(&demand_id)?;
    window(&input.playback_id, input.position_ms, input.playback_rate)?;
    let generation = number(&input.demand_generation, true)?
        .checked_sub(1)
        .filter(|n| *n > 0)
        .ok_or_else(invalid)?;
    let request_digest = config::request_digest(
        "PUT",
        &format!("/subtitle-translation/operations/{operation_id}/demands/{demand_id}"),
        &input,
    )?;
    mutate(
        &state,
        user.id,
        input.request_id,
        request_digest,
        input.intent_expires_at_ms,
        TranslationAction::Demand {
            operation_id,
            expected_operation_epoch: number(&input.expected_operation_epoch, true)?,
            demand_id,
            expected_generation: generation,
            playback_id: input.playback_id,
            position_ms: input.position_ms,
            playback_rate: input.playback_rate,
            paused: input.paused,
            continuity_id: input.continuity_id,
        },
        "updated",
    )
    .await
}
pub async fn release(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    ApiPath((operation_id, demand_id)): ApiPath<(String, String)>,
    Json(input): Json<ReleaseRequest>,
) -> Result<Response, ApiError> {
    envelope(input.version, &input.request_id, input.intent_expires_at_ms)?;
    id(&operation_id)?;
    id(&demand_id)?;
    id(&input.playback_id)?;
    let generation = number(&input.demand_generation, true)?
        .checked_sub(1)
        .filter(|n| *n > 0)
        .ok_or_else(invalid)?;
    let request_digest = config::request_digest(
        "DELETE",
        &format!("/subtitle-translation/operations/{operation_id}/demands/{demand_id}"),
        &input,
    )?;
    mutate(
        &state,
        user.id,
        input.request_id,
        request_digest,
        input.intent_expires_at_ms,
        TranslationAction::Release {
            operation_id,
            expected_operation_epoch: number(&input.expected_operation_epoch, true)?,
            demand_id,
            expected_generation: generation,
            playback_id: input.playback_id,
        },
        "released",
    )
    .await
}
pub async fn stop(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    ApiPath(operation_id): ApiPath<String>,
    Json(input): Json<StopRequest>,
) -> Result<Response, ApiError> {
    envelope(input.version, &input.request_id, input.intent_expires_at_ms)?;
    id(&operation_id)?;
    let request_digest = config::request_digest(
        "POST",
        &format!("/subtitle-translation/operations/{operation_id}/stop"),
        &input,
    )?;
    mutate(
        &state,
        user.id,
        input.request_id,
        request_digest,
        input.intent_expires_at_ms,
        TranslationAction::Stop {
            operation_id,
            expected_operation_epoch: number(&input.expected_operation_epoch, true)?,
        },
        "stopped",
    )
    .await
}
pub async fn retry(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    ApiPath(operation_id): ApiPath<String>,
    Json(input): Json<RetryRequest>,
) -> Result<Response, ApiError> {
    id(&operation_id)?;
    let request_digest = config::request_digest(
        "POST",
        &format!("/subtitle-translation/operations/{operation_id}/retry"),
        &input,
    )?;
    let (request_id, expires, action, outcome) = match input {
        RetryRequest::Operation {
            version,
            request_id,
            intent_expires_at_ms,
            expected_operation_epoch,
            expected_view_revision,
            expected_state,
            playback_id,
            position_ms,
            playback_rate,
            paused,
            continuity_id,
        } => {
            envelope(version, &request_id, intent_expires_at_ms)?;
            window(&playback_id, position_ms, playback_rate)?;
            if !matches!(expected_state.as_str(), "stopped" | "failed") {
                return Err(invalid());
            }
            (
                request_id,
                intent_expires_at_ms,
                TranslationAction::RetryOperation {
                    operation_id,
                    expected_operation_epoch: number(&expected_operation_epoch, true)?,
                    expected_view_revision: number(&expected_view_revision, true)?,
                    expected_state,
                    playback_id,
                    position_ms,
                    playback_rate,
                    paused,
                    continuity_id,
                },
                "resumed",
            )
        }
        RetryRequest::Range {
            version,
            request_id,
            intent_expires_at_ms,
            expected_operation_epoch,
            expected_operation_state,
            core_index,
            expected_range_revision,
            expected_range_state,
            playback_id,
            position_ms,
            playback_rate,
            paused,
            continuity_id,
        } => {
            envelope(version, &request_id, intent_expires_at_ms)?;
            window(&playback_id, position_ms, playback_rate)?;
            if expected_operation_state != "partial"
                || expected_range_state != "failed"
                || !(0..2048).contains(&core_index)
            {
                return Err(invalid());
            }
            (
                request_id,
                intent_expires_at_ms,
                TranslationAction::RetryRange {
                    operation_id,
                    expected_operation_epoch: number(&expected_operation_epoch, true)?,
                    core_index,
                    expected_range_revision: number(&expected_range_revision, true)?,
                    playback_id,
                    position_ms,
                    playback_rate,
                    paused,
                    continuity_id,
                },
                "range_retry_queued",
            )
        }
    };
    mutate(
        &state,
        user.id,
        request_id,
        request_digest,
        expires,
        action,
        outcome,
    )
    .await
}

fn operation_state(state: &str) -> Result<(), ApiError> {
    if !matches!(
        state,
        "checking_source"
            | "waiting_model"
            | "waiting_capacity"
            | "translating"
            | "partial"
            | "suspended"
            | "stopped"
            | "complete"
            | "failed"
    ) {
        return Err(observation_error());
    }
    Ok(())
}
fn safe_reason(reason: Option<&str>) -> Option<&'static str> {
    Some(match reason? {
        "disabled" => "disabled",
        "engine_unavailable" => "engine_unavailable",
        "model_unavailable" => "model_unavailable",
        "model_self_test_failed" => "model_self_test_failed",
        "model_missing" => "model_missing",
        "translation_unverified" => "translation_unverified",
        "source_unverified" => "source_unverified",
        "source_changed" => "source_changed",
        "source_clock_unsupported" => "source_clock_unsupported",
        "direct_audio_selection_unverified" => "direct_audio_selection_unverified",
        "delivery_unsupported" => "delivery_unsupported",
        "waiting_for_worker" | "worker_source_unavailable" => "waiting_for_worker",
        "waiting_for_playback_capacity" => "waiting_for_playback_capacity",
        "demand_expired" => "demand_expired",
        "worker_interrupted" => "worker_interrupted",
        "inference_failed" => "inference_failed",
        "inference_timeout" => "inference_timeout",
        "output_invalid" => "output_invalid",
        "segment_too_large" | "output_too_large" => "segment_too_large",
        "operation_stopped" => "operation_stopped",
        "epoch_conflict" => "epoch_conflict",
        "quota_exceeded" => "quota_exceeded",
        _ => "worker_interrupted",
    })
}
fn job_projection(
    job: &BackgroundJob,
    operation_id: &str,
    epoch: i64,
    expected_fence: i64,
) -> Result<Option<Value>, ApiError> {
    if !matches!(
        job.state,
        JobState::Queued | JobState::Running | JobState::Cancelling
    ) || job.fence != expected_fence
    {
        return Ok(None);
    }
    let (kind, core) = match job.supported_payload()? {
        JobPayload::SubtitleTranslationPrepare {
            operation_id: id,
            operation_epoch,
            ..
        } if id == operation_id && operation_epoch == epoch => ("translation_prepare", None),
        JobPayload::SubtitleTranslationRange { recipe, .. }
            if recipe.operation_id == operation_id && recipe.operation_epoch == epoch =>
        {
            ("translation_range", Some(recipe.core_index))
        }
        _ => return Ok(None),
    };
    let checkpoint = job.checkpoint.as_ref().filter(|row| {
        matches!(job.state, JobState::Running | JobState::Cancelling)
            && job.token.as_ref().is_some_and(|token| {
                token.fence == job.fence && token.lease_expires_ms > crate::state::clock_ms()
            })
            && row["fence"].as_i64() == Some(job.fence)
    });
    let phase = checkpoint
        .and_then(|row| row["phase"].as_str())
        .map(|phase| {
            if phase == "transcribing" {
                "translating"
            } else {
                phase
            }
        })
        .filter(|phase| {
            matches!(
                *phase,
                "checking_source" | "decoding" | "translating" | "validating" | "publishing"
            )
        });
    Ok(Some(
        json!({"job_id":job.id,"kind":kind,"state":job.state,"attempt":job.fence.to_string(),"fence":job.fence.to_string(),
        "phase":phase,"core_index":core,"owner_node_id":job.token.as_ref().map(|token|&token.node_id),"cancellable":true}),
    ))
}
async fn operation_projection(state: &AppState, row: &Value) -> Result<Value, ApiError> {
    let operation_id = field(row, "operation_id")?;
    id_check(operation_id)?;
    let epoch = counter(row, "operation_epoch", true)?;
    let view = counter(row, "view_revision", true)?;
    let state_name = field(row, "state")?;
    operation_state(state_name)?;
    let profile: TranslationProfile =
        serde_json::from_value(row["profile"].clone()).map_err(|_| observation_error())?;
    let mut active = Value::Null;
    let mut read = "0".to_owned();
    let total = counter(row, "source_size", true)?;
    if let Some(job_id) = row["active_job"]["job_id"].as_str() {
        let fence =
            number(field(&row["active_job"], "fence")?, false).map_err(|_| observation_error())?;
        id_check(job_id)?;
        if let Some(job) = state.store.background_job(job_id).await? {
            active = job_projection(&job, operation_id, number(&epoch, true)?, fence)?
                .unwrap_or(Value::Null);
            if active["phase"] == "checking_source" {
                if let Some(checkpoint) = job
                    .checkpoint
                    .as_ref()
                    .filter(|c| c["fence"].as_i64() == Some(fence))
                {
                    if let Some(bytes) = checkpoint["bytes_read"].as_str() {
                        if let Ok(bytes) = number(bytes, false) {
                            if bytes <= number(&total, true)? {
                                read = bytes.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    let asset = nullable_digest(row, "asset_id")?;
    let manifest = if row["manifest_revision"].is_null() {
        Value::Null
    } else {
        counter(row, "manifest_revision", false)?.into()
    };
    let audio = row["audio_index"]
        .as_i64()
        .filter(|n| (0..=255).contains(n))
        .ok_or_else(observation_error)?;
    let spoken = field(row, "spoken_language")?;
    if !languages().iter().any(|language| language == spoken) {
        return Err(observation_error());
    }
    let mtime = field(row, "source_mtime")?;
    if mtime.parse::<i64>().map(|n| n.to_string()).ok().as_deref() != Some(mtime) {
        return Err(observation_error());
    }
    let content = row["content_revision"]
        .as_str()
        .filter(|value| valid_digest(value));
    if !asset.is_null() && content.is_none() {
        return Err(observation_error());
    }
    let capacity_wait =
        active["state"] == "queued" && !state.transcode.speech_capacity_observation().await.2;
    let source_state = if content.is_some() {
        read = total.clone();
        "verified"
    } else if state_name == "failed" {
        "failed"
    } else {
        "checking"
    };
    Ok(
        json!({"version":1,"operation_id":operation_id,"operation_epoch":epoch,"view_revision":view,"state":if capacity_wait{"waiting_capacity"}else{state_name},
        "reason":if capacity_wait{Some("waiting_for_playback_capacity")}else{safe_reason(row["reason"].as_str())},"retryable":row["retryable"].as_bool().ok_or_else(observation_error)?,
        "asset_id":asset,"manifest_revision":manifest,"file_id":counter(row,"file_id",true)?,
        "source_size":total,"source_mtime":mtime,"content_revision":content,"audio_index":audio,
        "spoken_language":spoken,"output_language":profile.task.output_language(spoken),"task":profile.task.name(),"model_id":profile.model_id,
        "model_sha256":profile.model_sha256,"profile_id":profile.id()?,"timeline_contract":profile.timeline_contract,
        "source_check":{"state":source_state,"bytes_read":read,"total_bytes":counter(row,"source_size",true)?},
        "active_job":active,"failed_ranges":[],"failed_ranges_next_cursor":null,"server_now_ms":crate::state::clock_ms()}),
    )
}
pub async fn operation(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(operation_id): ApiPath<String>,
) -> Result<Response, ApiError> {
    let row = operation_row(&state, user.id, &operation_id).await?;
    let mut answer = operation_projection(&state, &row).await?;
    if let Some(asset) = row["asset_id"].as_str() {
        super::authorize_asset_source(&state, asset).await?;
        let lease = acquire_lease(&state, user.id, asset).await?;
        let (ranges, next) = failure_page(
            &state,
            user.id,
            &operation_id,
            &lease,
            -1,
            number(field(&row, "operation_epoch")?, true)?,
            number(field(&row, "view_revision")?, true)?,
        )
        .await?;
        answer["failed_ranges"] = json!(ranges);
        answer["failed_ranges_next_cursor"] = json!(next);
    }
    response(answer)
}

#[derive(Debug, Clone)]
struct Lease {
    asset_id: String,
    id: String,
    revision: String,
    expires: i64,
}
fn expired() -> ApiError {
    config::failure(
        StatusCode::GONE,
        "translation_snapshot_expired",
        crate::state::clock_ms(),
    )
}
fn lease_value(value: Value, asset_id: &str) -> Result<Lease, ApiError> {
    let id = field(&value, "lease_id")?.to_owned();
    id_check(&id)?;
    let revision = counter(&value, "manifest_revision", false)?;
    let expires = value["expires_at_ms"]
        .as_i64()
        .filter(|n| *n > crate::state::clock_ms())
        .ok_or_else(expired)?;
    Ok(Lease {
        asset_id: asset_id.into(),
        id,
        revision,
        expires,
    })
}
fn id_check(value: &str) -> Result<(), ApiError> {
    id(value).map_err(|_| observation_error())
}
async fn acquire_lease(state: &AppState, user_id: i64, asset_id: &str) -> Result<Lease, ApiError> {
    let value = state
        .store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id,
            asset_id: asset_id.into(),
            now_ms: crate::state::clock_ms(),
        })
        .await?
        .ok_or_else(|| {
            config::failure(
                StatusCode::TOO_MANY_REQUESTS,
                "quota_exceeded",
                crate::state::clock_ms(),
            )
        })?;
    lease_value(value, asset_id)
}
async fn read_lease(
    state: &AppState,
    user_id: i64,
    asset_id: &str,
    lease_id: &str,
) -> Result<Lease, ApiError> {
    id(lease_id)?;
    let mut rows = read_rows(
        state,
        user_id,
        TranslationQuery::Lease {
            asset_id: asset_id.into(),
            lease_id: lease_id.into(),
        },
    )
    .await?;
    lease_value(rows.pop().ok_or_else(expired)?, asset_id)
}
fn lease_header(headers: &HeaderMap) -> Result<&str, ApiError> {
    let value = headers
        .get(LEASE_HEADER)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(invalid)?;
    id(value)?;
    Ok(value)
}
async fn asset_row(
    state: &AppState,
    user_id: i64,
    asset_id: &str,
) -> Result<AssetSummary, ApiError> {
    if !valid_digest(asset_id) {
        return Err(invalid());
    }
    super::authorize_asset_source(state, asset_id).await?;
    let mut rows = read_rows(
        state,
        user_id,
        TranslationQuery::Asset {
            asset_id: asset_id.into(),
        },
    )
    .await?;
    asset_summary(rows.pop().ok_or(ApiError::NotFound("translation asset"))?)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentRow {
    segment_id: String,
    digest: String,
    core_index: i64,
    start_ms: i64,
    end_ms: i64,
}
#[derive(Debug, Serialize)]
struct SegmentReference {
    segment_id: String,
    digest: String,
    start_ms: i64,
    end_ms: i64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    start_ms: i64,
    end_ms: i64,
}
#[derive(Debug, Serialize)]
struct FailedRange {
    core_index: i64,
    range_revision: String,
    reason: Option<&'static str>,
    retryable: bool,
    attempts: i64,
}
fn source_range(start: i64, end: i64, duration: i64) -> Result<(), ApiError> {
    if start < 0 || end <= start || end > duration || duration > 14_400_000 {
        return Err(observation_error());
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn segment_page(
    state: &AppState,
    user_id: i64,
    asset: &AssetSummary,
    lease: &Lease,
    after: i64,
    through: i64,
    low: i64,
    high: i64,
) -> Result<(Vec<SegmentReference>, Option<String>), ApiError> {
    let rows = read_rows(
        state,
        user_id,
        TranslationQuery::Segments {
            asset_id: asset.asset_id.clone(),
            lease_id: lease.id.clone(),
            after_core: after,
            through_core: through,
        },
    )
    .await?;
    if rows.len() > 65 {
        return Err(observation_error());
    }
    let more = rows.len() > 64;
    let mut entries = Vec::new();
    let mut last = after;
    for row in rows.into_iter().take(64) {
        let row: SegmentRow = serde_json::from_value(row).map_err(|_| observation_error())?;
        source_range(row.start_ms, row.end_ms, asset.duration_ms)?;
        if !valid_digest(&row.segment_id)
            || !valid_digest(&row.digest)
            || row.core_index <= last
            || row.core_index > through
        {
            return Err(observation_error());
        }
        last = row.core_index;
        if row.end_ms <= low || row.start_ms >= high {
            continue;
        }
        entries.push(SegmentReference {
            segment_id: row.segment_id,
            digest: row.digest,
            start_ms: row.start_ms,
            end_ms: row.end_ms,
        });
    }
    let next = if more {
        let mut c = cursor("segments", user_id);
        c.asset_id = Some(asset.asset_id.clone());
        c.lease_id = Some(lease.id.clone());
        c.snapshot_revision = Some(lease.revision.clone());
        c.after = last.to_string();
        c.through = Some(through);
        c.window_start_ms = Some(low);
        c.window_end_ms = Some(high);
        Some(encode_cursor(&c)?)
    } else {
        None
    };
    Ok((entries, next))
}
async fn coverage_page(
    state: &AppState,
    user_id: i64,
    asset: &AssetSummary,
    lease: &Lease,
    after: i64,
    low: i64,
    high: i64,
) -> Result<(Vec<Coverage>, Option<String>), ApiError> {
    let rows = read_rows(
        state,
        user_id,
        TranslationQuery::Coverage {
            asset_id: asset.asset_id.clone(),
            lease_id: lease.id.clone(),
            after_start_ms: after,
            window_start_ms: low,
            window_end_ms: high,
        },
    )
    .await?;
    if rows.len() > 65 {
        return Err(observation_error());
    }
    let more = rows.len() > 64;
    let mut entries = Vec::new();
    let mut last = after;
    for row in rows.into_iter().take(64) {
        let row: Coverage = serde_json::from_value(row).map_err(|_| observation_error())?;
        source_range(row.start_ms, row.end_ms, asset.duration_ms)?;
        if row.start_ms <= last || row.end_ms <= low || row.start_ms >= high {
            return Err(observation_error());
        }
        last = row.start_ms;
        entries.push(row);
    }
    let next = if more {
        let mut c = cursor("coverage", user_id);
        c.asset_id = Some(asset.asset_id.clone());
        c.lease_id = Some(lease.id.clone());
        c.snapshot_revision = Some(lease.revision.clone());
        c.after = last.to_string();
        c.window_start_ms = Some(low);
        c.window_end_ms = Some(high);
        Some(encode_cursor(&c)?)
    } else {
        None
    };
    Ok((entries, next))
}
async fn failure_page(
    state: &AppState,
    user_id: i64,
    operation_id: &str,
    lease: &Lease,
    after: i64,
    epoch: i64,
    view: i64,
) -> Result<(Vec<FailedRange>, Option<String>), ApiError> {
    let rows = read_rows(
        state,
        user_id,
        TranslationQuery::FailedRanges {
            operation_id: operation_id.into(),
            lease_id: lease.id.clone(),
            after_core: after,
            expected_operation_epoch: epoch,
            expected_view_revision: view,
        },
    )
    .await?;
    if rows.len() > 17 {
        return Err(observation_error());
    }
    let more = rows.len() > 16;
    let mut entries = Vec::new();
    let mut last = after;
    for row in rows.into_iter().take(16) {
        let core = row["core_index"]
            .as_i64()
            .filter(|n| *n > last && *n < 2048)
            .ok_or_else(observation_error)?;
        last = core;
        entries.push(FailedRange {
            core_index: core,
            range_revision: counter(&row, "revision", true)?,
            reason: safe_reason(row["reason"].as_str()),
            retryable: row["retryable"].as_bool().ok_or_else(observation_error)?,
            attempts: row["attempts"]
                .as_i64()
                .filter(|n| *n >= 0 && *n <= 3)
                .ok_or_else(observation_error)?,
        });
    }
    let next = if more {
        let mut c = cursor("failures", user_id);
        c.operation_id = Some(operation_id.into());
        c.asset_id = Some(lease.asset_id.clone());
        c.lease_id = Some(lease.id.clone());
        c.snapshot_revision = Some(lease.revision.clone());
        c.epoch = Some(epoch.to_string());
        c.view_revision = Some(view.to_string());
        c.after = last.to_string();
        Some(encode_cursor(&c)?)
    } else {
        None
    };
    Ok((entries, next))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestQuery {
    position_ms: i64,
    renew_lease: Option<u8>,
}
pub async fn manifest(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(asset_id): ApiPath<String>,
    headers: HeaderMap,
    Query(query): Query<ManifestQuery>,
) -> Result<Response, ApiError> {
    ms(query.position_ms)?;
    if query.renew_lease.is_some_and(|v| v != 1) {
        return Err(invalid());
    }
    let asset = asset_row(&state, user.id, &asset_id).await?;
    let profile = super::profile::profiles()
        .into_iter()
        .find(|p| p.id().is_ok_and(|id| id == asset.profile_id))
        .ok_or_else(observation_error)?;
    let operation = operation_row(&state, user.id, &asset.operation_id).await?;
    let mut reused = false;
    let lease = if query.renew_lease.is_none() && headers.contains_key(LEASE_HEADER) {
        let old = read_lease(&state, user.id, &asset_id, lease_header(&headers)?).await?;
        if old.revision == asset.manifest_revision {
            reused = true;
            old
        } else {
            acquire_lease(&state, user.id, &asset_id).await?
        }
    } else {
        acquire_lease(&state, user.id, &asset_id).await?
    };
    let epoch = counter(&operation, "operation_epoch", true)?;
    let view = counter(&operation, "view_revision", true)?;
    let operation_state = field(&operation, "state")?;
    self::operation_state(operation_state)?;
    let etag = format!(
        "\"{}\"",
        plurx_core::store::subtitle_translation::digest(&json!([
            user.id.to_string(),
            asset_id,
            lease.revision,
            epoch,
            view,
            lease.id,
            field(&operation, "state")?,
            query.position_ms
        ]))?
    );
    if reused
        && headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            == Some(&etag)
    {
        let now = crate::state::clock_ms();
        if lease.expires <= now {
            return Err(expired());
        }
        let mut answer = Response::new(Body::empty());
        *answer.status_mut() = StatusCode::NOT_MODIFIED;
        private_headers(&mut answer, now, Some(lease.expires))?;
        answer.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag).map_err(|_| observation_error())?,
        );
        return Ok(answer);
    }
    let low = (query.position_ms - 30_000)
        .max(0)
        .min(asset.duration_ms.saturating_sub(1));
    let high = query
        .position_ms
        .saturating_add(90_000)
        .min(asset.duration_ms)
        .max(low + 1);
    let width = i64::from(profile.core_ms);
    let after = (low / width) - 1;
    let through = ((high - 1) / width).min(after + 8).min(2047);
    let (segments, segment_next_cursor) =
        segment_page(&state, user.id, &asset, &lease, after, through, low, high).await?;
    let (coverage, coverage_next_cursor) =
        coverage_page(&state, user.id, &asset, &lease, -1, low, high).await?;
    // A new Stop view must not be hidden by reading pages from an earlier view.
    let operation = operation_row(&state, user.id, &asset.operation_id).await?;
    let now = crate::state::clock_ms();
    if lease.expires <= now {
        return Err(expired());
    }
    let mut answer = response(
        json!({"version":1,"asset_id":asset.asset_id,"file_id":asset.file_id,"content_revision":asset.content_revision,
        "profile_id":asset.profile_id,"audio_index":asset.audio_index,"spoken_language":asset.spoken_language,"output_language":asset.output_language,"task":asset.task.name(),"timeline_contract":asset.timeline_contract,
        "manifest_revision":lease.revision,"view_revision":counter(&operation,"view_revision",true)?,
        "operation":{"operation_id":asset.operation_id,"epoch":counter(&operation,"operation_epoch",true)?,"state":field(&operation,"state")?,"reason":safe_reason(operation["reason"].as_str()),"retry_after_ms":null},
        "snapshot":{"lease_id":lease.id,"revision":lease.revision,"expires_in_ms":lease.expires-now},"coverage":coverage,"segments":segments,
        "coverage_next_cursor":coverage_next_cursor,"segment_next_cursor":segment_next_cursor,"server_now_ms":now}),
    )?;
    // Derive the tag from the final live view, not the pre-page observation.
    let etag = format!(
        "\"{}\"",
        plurx_core::store::subtitle_translation::digest(&json!([
            user.id.to_string(),
            asset_id,
            lease.revision,
            counter(&operation, "operation_epoch", true)?,
            counter(&operation, "view_revision", true)?,
            lease.id,
            field(&operation, "state")?,
            query.position_ms
        ]))?
    );
    answer.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| observation_error())?,
    );
    private_headers(&mut answer, now, Some(lease.expires))?;
    Ok(answer)
}
fn signed(value: &str, min: i64, max: i64) -> Result<i64, ApiError> {
    let n = value.parse::<i64>().map_err(|_| invalid())?;
    if n.to_string() != value || n < min || n > max {
        return Err(invalid());
    }
    Ok(n)
}
async fn page_lease(
    state: &AppState,
    user_id: i64,
    asset_id: &str,
    headers: &HeaderMap,
    c: &Cursor,
) -> Result<(AssetSummary, Lease), ApiError> {
    if c.asset_id.as_deref() != Some(asset_id)
        || c.lease_id.as_deref() != Some(lease_header(headers)?)
    {
        return Err(expired());
    }
    let asset = asset_row(state, user_id, asset_id).await?;
    let lease = read_lease(state, user_id, asset_id, lease_header(headers)?).await?;
    if c.snapshot_revision.as_deref() != Some(&lease.revision) {
        return Err(expired());
    }
    Ok((asset, lease))
}
pub async fn segments(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(asset_id): ApiPath<String>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let c = decode_cursor(&query.cursor, "segments", user.id)?;
    let (asset, lease) = page_lease(&state, user.id, &asset_id, &headers, &c).await?;
    let after = signed(&c.after, -1, 2047)?;
    let through = c
        .through
        .filter(|n| *n >= after && *n <= 2047 && *n - after <= 8)
        .ok_or_else(invalid)?;
    let low = ms(c.window_start_ms.ok_or_else(invalid)?)?;
    let high = ms(c.window_end_ms.ok_or_else(invalid)?)?;
    if high <= low {
        return Err(invalid());
    }
    let (entries, next_cursor) =
        segment_page(&state, user.id, &asset, &lease, after, through, low, high).await?;
    let mut answer = response(
        json!({"version":1,"asset_id":asset_id,"snapshot_revision":lease.revision,"entries":entries,"next_cursor":next_cursor}),
    )?;
    if lease.expires <= crate::state::clock_ms() {
        return Err(expired());
    }
    private_headers(&mut answer, crate::state::clock_ms(), Some(lease.expires))?;
    Ok(answer)
}
pub async fn coverage(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(asset_id): ApiPath<String>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let c = decode_cursor(&query.cursor, "coverage", user.id)?;
    let (asset, lease) = page_lease(&state, user.id, &asset_id, &headers, &c).await?;
    let low = ms(c.window_start_ms.ok_or_else(invalid)?)?;
    let high = ms(c.window_end_ms.ok_or_else(invalid)?)?;
    if high <= low || high - low > 120_000 {
        return Err(invalid());
    }
    let (entries, next_cursor) = coverage_page(
        &state,
        user.id,
        &asset,
        &lease,
        signed(&c.after, -1, 14_400_000)?,
        low,
        high,
    )
    .await?;
    let mut answer = response(
        json!({"version":1,"asset_id":asset_id,"snapshot_revision":lease.revision,"entries":entries,"next_cursor":next_cursor}),
    )?;
    if lease.expires <= crate::state::clock_ms() {
        return Err(expired());
    }
    private_headers(&mut answer, crate::state::clock_ms(), Some(lease.expires))?;
    Ok(answer)
}
pub async fn failed_ranges(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath(operation_id): ApiPath<String>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let c = decode_cursor(&query.cursor, "failures", user.id)?;
    if c.operation_id.as_deref() != Some(&operation_id) {
        return Err(expired());
    }
    let asset_id = c.asset_id.as_deref().ok_or_else(invalid)?;
    let (_, lease) = page_lease(&state, user.id, asset_id, &headers, &c).await?;
    let epoch = number(c.epoch.as_deref().ok_or_else(invalid)?, true)?;
    let view = number(c.view_revision.as_deref().ok_or_else(invalid)?, true)?;
    let row = operation_row(&state, user.id, &operation_id).await?;
    if row["operation_epoch"].as_str() != Some(&epoch.to_string())
        || row["view_revision"].as_str() != Some(&view.to_string())
        || row["asset_id"].as_str() != Some(asset_id)
    {
        return Err(expired());
    }
    let (entries, next_cursor) = failure_page(
        &state,
        user.id,
        &operation_id,
        &lease,
        signed(&c.after, -1, 2047)?,
        epoch,
        view,
    )
    .await?;
    let row = operation_row(&state, user.id, &operation_id).await?;
    if row["operation_epoch"].as_str() != Some(&epoch.to_string())
        || row["view_revision"].as_str() != Some(&view.to_string())
        || lease.expires <= crate::state::clock_ms()
    {
        return Err(expired());
    }
    let mut answer = response(
        json!({"version":1,"operation_id":operation_id,"operation_epoch":epoch.to_string(),"view_revision":view.to_string(),"entries":entries,"next_cursor":next_cursor}),
    )?;
    private_headers(&mut answer, crate::state::clock_ms(), Some(lease.expires))?;
    Ok(answer)
}
pub async fn segment(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    ApiPath((asset_id, segment_id)): ApiPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !valid_digest(&segment_id) {
        return Err(invalid());
    }
    asset_row(&state, user.id, &asset_id).await?;
    let lease = read_lease(&state, user.id, &asset_id, lease_header(&headers)?).await?;
    let mut rows = read_rows(
        &state,
        user.id,
        TranslationQuery::Segment {
            asset_id,
            lease_id: lease.id,
            segment_id,
        },
    )
    .await?;
    let row = rows.pop().ok_or_else(|| {
        config::failure(
            StatusCode::NOT_FOUND,
            "segment_unavailable",
            crate::state::clock_ms(),
        )
    })?;
    let body = field(&row, "body")?;
    let digest = field(&row, "digest")?;
    if body.len() > 16_384
        || !valid_digest(digest)
        || hex::encode(Sha256::digest(body.as_bytes())) != digest
    {
        return Err(config::failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "segment_unavailable",
            crate::state::clock_ms(),
        ));
    }
    let now = crate::state::clock_ms();
    if lease.expires <= now {
        return Err(expired());
    }
    // Raw canonical UTF-8 bytes, never deserialize/reserialize the inner body.
    let mut answer = Response::new(Body::from(body.to_owned()));
    answer.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    private_headers(&mut answer, now, Some(lease.expires))?;
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{background_jobs::BackgroundJobStore, SqliteStore};

    #[test]
    fn configured_defaults_preserve_saved_model_and_task_without_readiness() {
        let mut saved = config::Snapshot {
            version: 1,
            revision: "opaque".into(),
            enabled: true,
            model_id: None,
            profile_id: None,
            preferred_spoken_language: None,
            outcome: None,
            server_now_ms: 0,
        };
        assert_eq!(
            configured_default_profile(&saved),
            Some(super::super::profile::default_profile())
        );
        saved.model_id = Some("small".into());
        assert_eq!(
            configured_default_profile(&saved),
            Some(super::super::profile::selected_profile())
        );
        for profile in super::super::profile::profiles() {
            let summary = profile_summary(profile.clone(), Some("small")).expect("profile summary");
            assert_eq!(summary["default"], profile.model_id == "small");
        }
        let transcribe = super::super::profile::for_task(SpeechTask::Transcribe);
        saved.profile_id = Some(transcribe.id().expect("explicit task"));
        assert_eq!(configured_default_profile(&saved), Some(transcribe));
        saved.model_id = Some("base".into());
        assert!(
            configured_default_profile(&saved).is_none(),
            "unregistered model pair cannot choose a default"
        );
        saved.profile_id = Some("f".repeat(64));
        assert!(
            configured_default_profile(&saved).is_none(),
            "unknown explicit profile cannot choose a default"
        );
    }

    #[test]
    fn profile_projection_exposes_both_registered_small_task_defaults() {
        let profiles = super::super::profile::profiles()
            .into_iter()
            .map(|profile| {
                profile_summary(profile, Some("small")).expect("registered profile summary")
            })
            .collect::<Vec<_>>();
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            profiles
                .iter()
                .filter(|profile| profile["default"] == true)
                .count(),
            2
        );
        for profile in profiles {
            assert_eq!(profile["model_id"], "small");
            assert_eq!(profile["default"], true);
            assert!(profile["label"]
                .as_str()
                .is_some_and(|label| !label.is_empty()));
            let input = profile["input_languages"]
                .as_array()
                .expect("input languages");
            assert_eq!(
                input.len(),
                if profile["task"] == "translate" {
                    98
                } else {
                    99
                }
            );
            assert!(!input.iter().any(|language| language == "yue"));
            let selected = selected_profile(
                profile["model_id"].as_str().expect("model"),
                profile["profile_id"].as_str().expect("profile"),
            )
            .expect("exact selection");
            assert_eq!(
                Some(selected.model_sha256.as_str()),
                profile["model_sha256"].as_str()
            );
        }
    }

    fn create_input() -> CreateRequest {
        let profile = super::super::profile::selected_profile();
        CreateRequest {
            version: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            intent_expires_at_ms: 61_000,
            source_size: "100".into(),
            source_mtime: "20".into(),
            audio_index: 0,
            spoken_language: "de".into(),
            output_language: "en".into(),
            task: Some(SpeechTask::TranslateToEnglish),
            model_id: profile.model_id.clone(),
            profile_id: profile.id().expect("selected profile identity"),
            playback_id: uuid::Uuid::new_v4().to_string(),
            position_ms: 0,
            playback_rate: 1.0,
            paused: false,
            continuity_id: None,
        }
    }

    #[test]
    fn wire_digest_binds_route_identity_all_fields_and_action() {
        let mut input = create_input();
        let original = config::request_digest("POST", "/files/1/subtitle-translation", &input)
            .expect("wire digest");
        for (method, route) in [
            ("PUT", "/files/1/subtitle-translation"),
            ("POST", "/files/2/subtitle-translation"),
            ("POST", "/subtitle-translation/operations/1/stop"),
        ] {
            assert_ne!(
                original,
                config::request_digest(method, route, &input).expect("distinct route")
            );
        }
        input.paused = true;
        assert_ne!(
            original,
            config::request_digest("POST", "/files/1/subtitle-translation", &input)
                .expect("changed semantic field")
        );
        let mut value = serde_json::to_value(input).expect("request DTO");
        value["target_node_id"] = "worker".into();
        assert!(serde_json::from_value::<CreateRequest>(value).is_err());
    }

    #[tokio::test]
    async fn accepted_create_replays_before_changed_admission_and_checks_current_admin() {
        let dir = tempfile::tempdir().expect("isolated store");
        let path = dir.path().join("store.db");
        let store = SqliteStore::open(&path).expect("real schema");
        let conn = rusqlite::Connection::open(path).expect("fixture connection");
        conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','hash',1); INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media\"]'); INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie'); INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}'); INSERT INTO settings(key,value) VALUES('subtitles.translation.config.v1','{\"version\":1,\"enabled\":true}');").expect("real catalog setup");
        let input = create_input();
        let digest = config::request_digest("POST", "/files/1/subtitle-translation", &input)
            .expect("original wire digest");
        assert!(config::replay(&store, 1, &input.request_id, &digest, 1_000)
            .await
            .expect("no receipt")
            .is_none());
        let first = store
            .mutate_subtitle_translation(TranslationMutation {
                user_id: 1,
                request_id: input.request_id.clone(),
                request_digest: digest.clone(),
                intent_expires_at_ms: input.intent_expires_at_ms,
                now_ms: 1_000,
                action: TranslationAction::Create {
                    source: SourceRevision {
                        file_id: 1,
                        source_size: 100,
                        source_mtime: 20,
                    },
                    profile: super::super::profile::selected_profile(),
                    audio_index: 0,
                    spoken_language: "de".into(),
                    duration_ms: 61000,
                    target_node_id: "worker".into(),
                    playback_id: input.playback_id.clone(),
                    position_ms: 0,
                    playback_rate: 1.0,
                    paused: false,
                    continuity_id: None,
                },
            })
            .await
            .expect("atomic accepted preparation");
        assert_eq!(first["outcome"], "accepted");
        conn.execute_batch("UPDATE files SET size=101,mtime=21,probe_json=NULL WHERE id=1; UPDATE settings SET value='{\"version\":1,\"enabled\":false}' WHERE key='subtitles.translation.config.v1';").expect("source and admission change after lost response");
        let replay = config::replay(&store, 1, &input.request_id, &digest, 70_000)
            .await
            .expect("retained replay after first-use expiry")
            .expect("durable receipt");
        assert_eq!(
            mutation_receipt(&first, "accepted").expect("first safe receipt"),
            mutation_receipt(&replay, "accepted").expect("exact safe replay")
        );
        let changed = config::request_digest("POST", "/files/2/subtitle-translation", &input)
            .expect("different route");
        assert_eq!(
            config::replay(&store, 1, &input.request_id, &changed, 70_001)
                .await
                .expect_err("cross-route UUID reuse refuses")
                .code(),
            Some("request_conflict")
        );
        conn.execute("UPDATE users SET is_admin=0 WHERE id=1", [])
            .expect("authority revoked");
        assert_eq!(
            config::replay(&store, 1, &input.request_id, &digest, 70_002)
                .await
                .expect_err("replay still needs current admin")
                .code(),
            Some("permission_denied")
        );
    }

    #[test]
    fn cursor_rejects_cross_account_kind_and_noncanonical_or_open_shape() {
        let mut page = cursor("coverage", 1);
        page.asset_id = Some("a".repeat(64));
        page.lease_id = Some(uuid::Uuid::new_v4().to_string());
        page.snapshot_revision = Some("9007199254740993".into());
        page.after = "30".into();
        let encoded = encode_cursor(&page).expect("bounded cursor");
        assert!(decode_cursor(&encoded, "coverage", 1).is_ok());
        assert!(decode_cursor(&encoded, "coverage", 2).is_err());
        assert!(decode_cursor(&encoded, "segments", 1).is_err());
        let mut value = serde_json::to_value(page).expect("cursor wire");
        value["path"] = "/private/secret".into();
        let open = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("bad cursor fixture"));
        assert!(decode_cursor(&open, "coverage", 1).is_err());
        value.as_object_mut().expect("cursor object").remove("path");
        value
            .as_object_mut()
            .expect("cursor object")
            .remove("file_id");
        let omitted =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("omitted nullable fixture"));
        assert!(decode_cursor(&omitted, "coverage", 1).is_err());
    }

    #[test]
    fn closed_worker_and_receipt_projections_exclude_internal_authority() {
        let operation = uuid::Uuid::new_v4().to_string();
        let receipt = json!({"operation_id":operation,"operation_epoch":"2","view_revision":"9007199254740993","state":"stopped","asset_id":null,"server_now_ms":1000,"target_node_id":"worker","source_path":"/private/source","profile":{"private":true},"token":"secret"});
        let safe = mutation_receipt(&receipt, "stopped").expect("closed stopped projection");
        assert_eq!(safe.as_object().expect("receipt").len(), 8);
        assert_eq!(safe["view_revision"], "9007199254740993");
        assert_eq!(safe["server_now_ms"], 1000);
        let workers = worker_rows(vec![json!({"node_id":"worker","label":"Local worker","current":true,"eligible":true,"observed_at_ms":1000,"expires_at_ms":31000,"engine":null,"capabilities":[{"model_id":"small","profile_id":"a".repeat(64),"status":"unverified_cache","reason":"translation_unverified","verified_at_ms":null,"model_sha256":"b".repeat(64),"engine_sha256":null,"model_path":"/private/model"}],"token":"secret"})]).expect("closed worker");
        let wire = serde_json::to_string(&workers).expect("worker JSON");
        assert!(!wire.contains("/private"));
        assert!(!wire.contains("token"));
        assert_eq!(workers[0]["capabilities"][0]["status"], "unverified_cache");
        let mut unknown = workers;
        unknown[0]["capabilities"][0]["status"] = "invented_ready".into();
        assert!(worker_rows(unknown).is_err());
    }
}

#[cfg(test)]
#[path = "api_http_tests.rs"]
mod lifecycle_tests;
