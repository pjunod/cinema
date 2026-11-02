//! Progressive foreign-speech translation over durable source and core fences.
pub(crate) mod api;
pub(crate) mod config;
mod installer;
mod manager;
pub(crate) mod profile;
pub(crate) use manager::worker_pass;
mod read_attestation;
pub(crate) mod runtime;

use crate::{http::error::ApiError, state::AppState, subtitle_models::artifacts};
use plurx_core::{
    domain::MediaFile,
    error::StoreError,
    store::{
        background_jobs::JobKind,
        subtitle_translation::TranslationProfile,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{Notify, RwLock};

pub(crate) struct Runtime {
    sources: RwLock<VecDeque<Arc<runtime::VerifiedSource>>>,
    reads: Arc<read_attestation::ReadAttestations>,
    pub(crate) wake: Notify,
    operation_cursor: RwLock<Option<String>>,
    candidate_cursor: RwLock<Option<plurx_core::store::background_jobs::CandidateCursor>>,
    turn: std::sync::atomic::AtomicUsize,
    maintenance_at: std::sync::atomic::AtomicI64,
    automatic_keys: RwLock<VecDeque<String>>,
    automatic_next: std::sync::atomic::AtomicI64,
    failures: RwLock<std::collections::BTreeMap<String, String>>,
}
impl Runtime {
    pub(crate) fn new() -> Self {
        Self {
            sources: RwLock::new(VecDeque::new()),
            reads: read_attestation::ReadAttestations::new(),
            operation_cursor: RwLock::new(None),
            candidate_cursor: RwLock::new(None),
            turn: std::sync::atomic::AtomicUsize::new(0),
            maintenance_at: std::sync::atomic::AtomicI64::new(0),
            automatic_keys: RwLock::new(VecDeque::new()),
            automatic_next: std::sync::atomic::AtomicI64::new(0),
            wake: Notify::new(),
            failures: RwLock::new(Default::default()),
        }
    }
    pub(crate) async fn remember(&self, source: Arc<runtime::VerifiedSource>) {
        let mut entries = self.sources.write().await;
        entries.retain(|v| v.revision().file_id != source.revision().file_id);
        entries.push_back(source);
        while entries.len() > 64 {
            entries.pop_front();
        }
    }
    pub(crate) async fn cached(&self, file: &MediaFile) -> Option<Arc<runtime::VerifiedSource>> {
        let candidate = self
            .sources
            .read()
            .await
            .iter()
            .find(|v| {
                v.revision().file_id == file.id
                    && v.revision().source_size == file.size
                    && v.revision().source_mtime == file.mtime
            })
            .cloned()?;
        if !candidate.unchanged() {
            return None;
        }
        let current = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::fragment_index_cluster::open_source_playback_fence(
                file,
                Some(candidate.object_version()),
            ),
        )
        .await
        .ok()?
        .ok()?;
        (plurx_core::fs_secure::std_file_identity(&current.handle).ok()
            == Some(candidate.identity()))
        .then_some(candidate)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    model: plurx_core::store::background_jobs_transcription::SubtitleModelReceipt,
    profile: TranslationProfile,
    profile_id: String,
    self_test_sha256: String,
}
pub(crate) async fn workers_public(state: &AppState) -> Result<Vec<Value>, ApiError> {
    let now = crate::state::clock_ms();
    let engine = artifacts::inspect_engine().await;
    let mut models = std::collections::BTreeMap::new();
    let receipts = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.store.read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: now,
            query: TranslationQuery::Capabilities {
                node_id: state.node_id.clone(),
            },
        }),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Worker readiness unavailable".into()))??;
    let failures = state.subtitle_models.translation.failures.read().await;
    let mut capabilities = Vec::new();
    for selected in profile::profiles() {
        let spec = profile::model_spec_for(&selected)?;
        // Both tasks share immutable bytes, while readiness remains per profile.
        // Observe each model separately once per response, never reuse Small's
        // metadata for Base or vice versa.
        if !models.contains_key(&spec.sha256) {
            models.insert(
                spec.sha256.clone(),
                artifacts::model_identity(&state.subtitle_models.root, &spec).await,
            );
        }
        let model = &models[&spec.sha256];
        let profile_id = selected.id()?;
        let mut verified = None;
        if let (Ok(engine), Ok(model)) = (&engine, model) {
            for row in &receipts {
                let Ok(receipt) = serde_json::from_value::<Capability>(row.clone()) else {
                    continue;
                };
                let m = &receipt.model;
                if receipt.profile == selected
                    && receipt.profile_id == profile_id
                    && receipt.self_test_sha256 == profile::SELF_TEST_SHA256
                    && m.node_id == state.node_id
                    && m.boot_id == state.subtitle_models.boot_id
                    && m.model_id == spec.id
                    && m.model_sha256 == spec.sha256
                    && m.adapter_contract == engine.adapter_contract
                    && m.engine_sha256 == engine.binary_sha256
                    && m.engine_file_identity == artifacts::identity_key(engine.identity)
                    && m.model_file_identity == artifacts::identity_key(*model)
                {
                    verified = Some(m.verified_at_ms);
                    break;
                }
            }
        }
        let failure = failures.get(&profile_id).map(String::as_str);
        let (status, reason) = if let Some(failure) = failure {
            ("verification_failed", Some(failure))
        } else if engine.is_err() {
            ("unknown", Some("engine_unavailable"))
        } else if model.as_ref().is_err_and(|e| e == "model_not_installed") {
            ("missing", Some("model_missing"))
        } else if model.is_err() {
            ("unknown", Some("model_unavailable"))
        } else if verified.is_some() {
            ("verified", None)
        } else {
            ("unverified_cache", Some("translation_unverified"))
        };
        capabilities.push(json!({"task":selected.task.name(),"model_id":spec.id,"profile_id":profile_id,"status":status,"reason":reason,"verified_at_ms":if status=="verified"{verified}else{None},"model_sha256":spec.sha256,"engine_sha256":engine.as_ref().ok().map(|e|e.binary_sha256.clone())}));
    }
    let (cpu_budget, cpu_used, capacity_available) =
        state.transcode.speech_capacity_observation().await;
    let eligible = state
        .jobs
        .execution_authority()
        .may_execute_job(JobKind::SubtitleTranslationRange)
        .await;
    Ok(vec![
        json!({"node_id":state.node_id,"label":state.server_name,"current":true,"eligible":eligible,"observed_at_ms":now,"expires_at_ms":now+30000,"engine":engine.as_ref().ok().map(|e|json!({"adapter_contract":e.adapter_contract,"binary_sha256":e.binary_sha256,"platform":e.platform})),"capacity":{"cpu_threads_required":2,"cpu_threads_budget":cpu_budget,"cpu_threads_used":cpu_used,"available":capacity_available,"reason":if capacity_available{None}else{Some("waiting_for_playback_capacity")}},"capabilities":capabilities}),
    ])
}

fn probe_duration_ms(stream: &Value) -> Option<i64> {
    if let Some(seconds) = stream["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
    {
        return (seconds.is_finite() && seconds > 0.0).then_some((seconds * 1000.0) as i64);
    }
    let tags = stream["tags"].as_object()?;
    let tag = tags
        .iter()
        .find(|(key, _)| {
            key.eq_ignore_ascii_case("duration")
                || key.to_ascii_lowercase().starts_with("duration-")
        })?
        .1
        .as_str()?;
    let mut parts = tag.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let seconds =
        h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds > 0.0).then_some((seconds * 1000.0) as i64)
}

/// Clock coordinates are milliseconds/samples after subtracting the container
/// origin, matching FFmpeg -copyts -start_at_zero and delivery media origins.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceClock {
    pub(crate) origin_samples: i64,
    pub(crate) audio_start_samples: i64,
    pub(crate) duration_ms: i64,
    pub(crate) source_index: Option<i64>,
}
fn rational_samples(numerator: i128, denominator: i128) -> Result<i64, &'static str> {
    if denominator <= 0 {
        return Err("source_clock_unsupported");
    }
    let rounded = numerator
        .checked_abs()
        .ok_or("source_clock_unsupported")?
        .checked_add(denominator / 2)
        .ok_or("source_clock_unsupported")?
        / denominator;
    let samples = rounded * numerator.signum();
    if samples.abs() > 604_800 * 16_000 {
        return Err("source_clock_unsupported");
    }
    i64::try_from(samples).map_err(|_| "source_clock_unsupported")
}
fn probe_samples(value: &Value) -> Result<i64, &'static str> {
    let raw = value.as_str().ok_or("source_clock_unsupported")?;
    let (negative, raw) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let mut parts = raw.split('.');
    let seconds = parts.next().ok_or("source_clock_unsupported")?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || seconds.is_empty()
        || fraction.len() > 9
        || !seconds
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err("source_clock_unsupported");
    }
    let denominator = 10_i128.pow(fraction.len() as u32);
    let seconds: i128 = seconds.parse().map_err(|_| "source_clock_unsupported")?;
    let fraction: i128 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| "source_clock_unsupported")?
    };
    let ticks = seconds
        .checked_mul(denominator)
        .and_then(|v| v.checked_add(fraction))
        .and_then(|v| v.checked_mul(16_000))
        .ok_or("source_clock_unsupported")?;
    rational_samples(if negative { -ticks } else { ticks }, denominator)
}
fn stream_start_samples(stream: &Value) -> Result<i64, &'static str> {
    // Prefer the stream's rational clock over a decimal ffprobe rendering.
    if let (Some(pts), Some(base)) = (stream["start_pts"].as_i64(), stream["time_base"].as_str()) {
        let (numerator, denominator) = base.split_once('/').ok_or("source_clock_unsupported")?;
        let numerator: i128 = numerator.parse().map_err(|_| "source_clock_unsupported")?;
        let denominator: i128 = denominator
            .parse()
            .map_err(|_| "source_clock_unsupported")?;
        if numerator <= 0 {
            return Err("source_clock_unsupported");
        }
        let ticks = i128::from(pts)
            .checked_mul(numerator)
            .and_then(|v| v.checked_mul(16_000))
            .ok_or("source_clock_unsupported")?;
        return rational_samples(ticks, denominator);
    }
    probe_samples(&stream["start_time"])
}
pub(crate) fn source_clock(
    value: &Value,
    ordinal: i64,
    container_ms: i64,
) -> Result<SourceClock, &'static str> {
    let streams = value["streams"]
        .as_array()
        .ok_or("source_clock_unsupported")?;
    if !(0..=255).contains(&ordinal) || !(1..=14_400_000).contains(&container_ms) {
        return Err("delivery_unsupported");
    }
    let origin_samples = probe_samples(&value["format"]["start_time"])?;
    let audio = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .nth(ordinal as usize)
        .ok_or("direct_audio_selection_unverified")?;
    let audio_start_samples = stream_start_samples(audio)? - origin_samples;
    // A stream wholly outside the bounded source timeline cannot supply cores.
    if audio_start_samples.abs() > 14_400_000 * 16 {
        return Err("source_clock_unsupported");
    }
    let duration_ms = probe_duration_ms(audio).map_or(container_ms, |duration| {
        (audio_start_samples.div_euclid(16) + duration).min(container_ms)
    });
    if duration_ms <= 0 {
        return Err("delivery_unsupported");
    }
    Ok(SourceClock {
        origin_samples,
        audio_start_samples,
        duration_ms,
        source_index: audio["index"].as_i64().filter(|n| *n >= 0),
    })
}
pub(crate) async fn selected_file_capability(
    state: &AppState,
    file: &MediaFile,
    ordinal: i64,
) -> Result<SourceClock, &'static str> {
    if !file
        .audio_streams
        .iter()
        .any(|audio| audio.index == ordinal)
    {
        return Err("direct_audio_selection_unverified");
    }
    if file.size <= 0 || file.video_codec.is_none() {
        return Err("delivery_unsupported");
    }
    let raw = state
        .store
        .get_file_probe_json(file.id)
        .await
        .map_err(|_| "source_unverified")?
        .ok_or("source_unverified")?;
    if raw.len() > 1024 * 1024 {
        return Err("source_clock_unsupported");
    }
    let value: Value = serde_json::from_str(&raw).map_err(|_| "source_clock_unsupported")?;
    if value["streams"].as_array().map(|streams| {
        streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .count()
    }) != Some(file.audio_streams.len())
    {
        return Err("source_changed");
    }
    source_clock(
        &value,
        ordinal,
        file.duration_ms.ok_or("delivery_unsupported")?,
    )
}
pub(crate) async fn eligible_worker(
    state: &AppState,
    file: &MediaFile,
    selected: &TranslationProfile,
) -> Result<String, ApiError> {
    // Create validated the selected audio and source clock before worker choice.
    if !profile::registered(selected) {
        return Err(ApiError::BadRequest(
            "Choose the current translation profile".into(),
        ));
    }
    let rows = workers_public(state).await?;
    if !rows.iter().any(|w| {
        w["eligible"] == true
            && w["capabilities"].as_array().is_some_and(|caps| {
                caps.iter().any(|c| {
                    c["status"] == "verified"
                        && c["profile_id"].as_str() == selected.id().ok().as_deref()
                })
            })
    }) {
        return Err(ApiError::typed(
            axum::http::StatusCode::CONFLICT,
            "translation_unverified",
            "Install and test the translation model in Settings",
        ));
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::fragment_index_cluster::open_source_playback_fence(file, None),
    )
    .await
    .map_err(|_| {
        ApiError::typed(
            axum::http::StatusCode::CONFLICT,
            "worker_source_unavailable",
            "The worker cannot access this source",
        )
    })?
    .map_err(|_| {
        ApiError::typed(
            axum::http::StatusCode::CONFLICT,
            "worker_source_unavailable",
            "The worker cannot access this source",
        )
    })?;
    Ok(state.node_id.clone())
}
pub(crate) async fn authorize_asset_source(
    state: &AppState,
    asset_id: &str,
) -> Result<(), ApiError> {
    let now = crate::state::clock_ms();
    let rows = state
        .store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: now,
            query: TranslationQuery::Asset {
                asset_id: asset_id.into(),
            },
        })
        .await?;
    let asset = rows
        .first()
        .ok_or(ApiError::NotFound("translation asset"))?;
    let file_id = asset["file_id"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or(ApiError::NotFound("translation asset"))?;
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let expected_size = asset["source_size"]
        .as_str()
        .and_then(|value| value.parse::<i64>().ok());
    let expected_mtime = asset["source_mtime"]
        .as_str()
        .and_then(|value| value.parse::<i64>().ok());
    if expected_size != Some(file.size) || expected_mtime != Some(file.mtime) {
        return Err(ApiError::typed(
            axum::http::StatusCode::GONE,
            "source_changed",
            "The source changed",
        ));
    }
    // Same authenticated local-catalog scope as /files/{id}/direct. Source
    // references and remote-library credentials never enter this local route.
    if let Some(source) = state.subtitle_models.translation.cached(&file).await {
        if asset["content_revision"].as_str() == Some(source.sha256()) {
            return Ok(());
        }
    }
    let content = asset["content_revision"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or(ApiError::NotFound("translation asset"))?;
    state
        .subtitle_models
        .translation
        .reads
        .authorize(state, &file, content)
        .await
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelRequest {
    version: u8,
    request_id: String,
    intent_expires_at_ms: i64,
    purpose: String,
    profile_id: String,
    node_ids: Vec<String>,
}
pub(crate) async fn model_operation(
    state: &AppState,
    id: &str,
    action: plurx_core::store::background_jobs_transcription::ModelAction,
    body: Value,
) -> Result<axum::Json<Value>, ApiError> {
    use plurx_core::store::background_jobs::{EnqueueJob, EnqueueOutcome, JobPayload, JobRequest};
    let request: ModelRequest = serde_json::from_value(body)
        .map_err(|_| ApiError::BadRequest("Invalid translation model request".into()))?;
    let selected = profile::profiles()
        .into_iter()
        .find(|p| p.id().is_ok_and(|id| id == request.profile_id))
        .ok_or_else(|| ApiError::BadRequest("Choose a current speech profile".into()))?;
    let spec = profile::model_spec_for(&selected)?;
    let now = crate::state::clock_ms();
    let unique = request
        .node_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if request.version != 2
        || request.purpose != "translation"
        || request.profile_id != selected.id()?
        || id != spec.id
        || uuid::Uuid::parse_str(&request.request_id).is_err()
        || request.node_ids.is_empty()
        || request.node_ids.len() > 16
        || unique.len() != request.node_ids.len()
        || request
            .node_ids
            .iter()
            .any(|n| n.is_empty() || n.len() > 128)
    {
        return Err(ApiError::BadRequest(
            "Choose the current translation model and distinct workers".into(),
        ));
    }
    let digest = plurx_core::store::subtitle_translation::digest(
        &json!({"method":"POST","route":format!("/subtitle-transcription/models/{id}/{}",if action==plurx_core::store::background_jobs_transcription::ModelAction::TranslationInstall{"install"}else{"verify"}),"body":request}),
    )?;
    if request.intent_expires_at_ms <= now || request.intent_expires_at_ms > now + 300000 {
        return Err(ApiError::typed(
            axum::http::StatusCode::CONFLICT,
            "intent_expired",
            "Refresh and try again",
        ));
    }
    let mut operations = Vec::new();
    for node in &request.node_ids {
        // Initial qualified route is the serving single worker. A caller cannot
        // use this setup endpoint to install on an unqualified remote node.
        if node != &state.node_id {
            operations.push(json!({"node_id":node,"job_id":null,"outcome":"worker_not_current"}));
            continue;
        }
        let key =
            plurx_core::store::subtitle_translation::digest(&json!([request.request_id, node]))?;
        let bytes =
            hex::decode(&key[..32]).map_err(|_| ApiError::Internal("request identity".into()))?;
        let request_id = uuid::Uuid::from_slice(&bytes)
            .map_err(|_| ApiError::Internal("request identity".into()))?
            .to_string();
        let payload = JobPayload::SubtitleModelOperation {
            action,
            translation_profile: Some(selected.clone()),
            model_id: spec.id.clone(),
            catalog_revision: crate::subtitle_models::catalog().revision.clone(),
            model_sha256: spec.sha256.clone(),
            adapter_contract: selected.adapter_contract.clone(),
            target_node_id: node.clone(),
        };
        let outcome = state
            .store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload,
                dedupe_key: format!("translation-model:{key}"),
                priority: 1,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "translation-model".into(),
                    request_id,
                    request_digest: digest.clone(),
                    consumer_kind: "subtitle_model_operation".into(),
                    consumer_ref: key,
                    target_node_id: Some(node.clone()),
                    deadline_ms: None,
                    retain_identity: true,
                },
            })
            .await?;
        let job_id = match &outcome {
            EnqueueOutcome::Accepted { job_id, .. } | EnqueueOutcome::Existing { job_id, .. } => {
                Some(job_id.clone())
            }
            _ => None,
        };
        let mut value =
            serde_json::to_value(outcome).map_err(|e| ApiError::Internal(e.to_string()))?;
        value["node_id"] = json!(node);
        value["job_id"] = json!(job_id);
        operations.push(value);
    }
    state
        .subtitle_models
        .translation
        .failures
        .write()
        .await
        .remove(&request.profile_id);
    state.subtitle_models.translation.wake.notify_one();
    Ok(axum::Json(
        json!({"version":2,"purpose":"translation","model_id":id,"profile_id":request.profile_id,"operations":operations,"intent_expires_at_ms":request.intent_expires_at_ms,"server_now_ms":now}),
    ))
}

#[cfg(test)]
mod duration_tests {
    use super::{probe_duration_ms, source_clock, stream_start_samples};
    use serde_json::json;

    #[test]
    fn selected_audio_uses_ordinal_and_normalizes_signed_container_origin() {
        let probe = json!({"format":{"start_time":"-1.500000"},"streams":[
            {"index":0,"codec_type":"video","start_time":"-1.500000"},
            {"index":2,"codec_type":"audio","start_time":"-1.500000","duration":"20"},
            {"index":5,"codec_type":"audio","start_pts":-48000,"time_base":"1/48000","start_time":"-1.000000","duration":"10"}
        ]});
        let clock = source_clock(&probe, 1, 20_000).expect("selected audio clock");
        assert_eq!(clock.origin_samples, -24_000);
        assert_eq!(clock.audio_start_samples, 8_000);
        assert_eq!(clock.source_index, Some(5));
        assert_eq!(clock.duration_ms, 10_500);
        assert!(source_clock(&probe, 2, 20_000).is_err());
    }

    #[test]
    fn rational_audio_clock_refuses_invalid_and_overflowing_metadata() {
        assert_eq!(
            stream_start_samples(&json!({"start_pts":1,"time_base":"1/48000"}))
                .expect("rounded sample"),
            0
        );
        assert!(stream_start_samples(&json!({"start_pts":1,"time_base":"1/0"})).is_err());
        assert!(stream_start_samples(
            &json!({"start_pts":i64::MIN,"time_base":"18446744073709551616/1"})
        )
        .is_err());
        assert!(stream_start_samples(&json!({"start_time":"NaN"})).is_err());
    }

    #[test]
    fn audio_timeline_reads_stream_seconds_or_matroska_tag() {
        assert_eq!(
            probe_duration_ms(&json!({"duration":"5400.000000"})),
            Some(5_400_000)
        );
        assert_eq!(
            probe_duration_ms(&json!({"tags":{"DURATION-eng":"01:30:00.200000000"}})),
            Some(5_400_200)
        );
        assert_eq!(probe_duration_ms(&json!({"tags":{"title":"x"}})), None);
        assert_eq!(probe_duration_ms(&json!({"duration":"N/A"})), None);
    }
}
