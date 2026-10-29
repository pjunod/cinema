//! Opt-in offline whisper.cpp jobs: fixed argv, bounded work, common queue Stop.
use crate::http::{error::ApiError, extract::AdminUser};
use crate::state::AppState;
use axum::{
    extract::{Path as ApiPath, State},
    Json,
};
use plurx_core::{
    domain::{DownloadedSubtitle, MediaFile, SubtitleTranscription},
    error::StoreError,
    store::{
        background_jobs::{
            CandidateQuery, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest,
            JobSettlement,
        },
        background_jobs_transcription::{artifact_key, valid_language},
        MAX_DOWNLOADED_SUBTITLE_BYTES,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const PREFIX: &str = "subtitles.transcription.";
const MAX_DURATION_MS: i64 = 4 * 60 * 60 * 1000;
const MAX_WAV_BYTES: u64 = 4 * 60 * 60 * 16_000 * 2 + 4096;
const MAX_MODEL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const WALL: Duration = Duration::from_secs(4 * 60 * 60);
// Keep the container-relative film clock. Hard compensation inserts silence
// for leading samples/gaps and drops overlaps; it never closes a gap by
// retiming speech. The WAV byte ceiling includes all inserted silence.
const PCM_CLOCK_FILTER: &str =
    "aresample=16000:async=1:first_pts=0:min_comp=0:min_hard_comp=0:max_soft_comp=0";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub interval_mins: u32,
    pub command: String,
    pub model_path: String,
    pub language: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_mins: 0,
            command: "whisper-cli".into(),
            model_path: String::new(),
            language: "en".into(),
        }
    }
}
impl Settings {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        if self.interval_mins > 10_080
            || !valid_language(&self.language)
            || self.command.is_empty()
            || self.command.len() > 4096
            || self.model_path.len() > 4096
            || self.command.contains(['\0', '\r', '\n'])
            || self.model_path.contains(['\0', '\r', '\n'])
            || (!self.model_path.is_empty() && !Path::new(&self.model_path).is_absolute())
        {
            return Err(ApiError::BadRequest("Choose a local command, absolute model path, language code and interval up to one week".into()));
        }
        Ok(())
    }
}
async fn read_settings(state: &AppState) -> Result<Settings, StoreError> {
    let value = state.store.get_setting(&format!("{PREFIX}config")).await?;
    match value {
        None => Ok(Settings::default()),
        Some(value) => {
            let mut config: Settings = serde_json::from_str(&value)
                .map_err(|_| StoreError::Task("invalid transcription settings".into()))?;
            config.language = normalized_language(&config.language);
            Ok(config)
        }
    }
}
fn executable(command: &str) -> Option<PathBuf> {
    let usable = |path: &Path| {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    };
    if Path::new(command).components().count() > 1 {
        return usable(Path::new(command)).then(|| PathBuf::from(command));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|root| root.join(command))
            .find(|path| usable(path))
    })
}
async fn readiness(config: &Settings) -> Value {
    let command_available = executable(&config.command).is_some();
    let model_available = if config.model_path.is_empty() {
        false
    } else {
        tokio::fs::symlink_metadata(&config.model_path)
            .await
            .is_ok_and(|meta| meta.is_file() && meta.len() > 0 && meta.len() <= MAX_MODEL_BYTES)
    };
    json!({"command_available":command_available,"model_available":model_available,"ready":command_available && model_available,
        "reason": if !command_available { "Install whisper.cpp or choose its local whisper-cli executable" } else if !model_available { "Choose an installed regular whisper.cpp model, up to 2 GiB; models are never downloaded" } else { "Local command and model are present; a fixture transcription is still required" }})
}
pub async fn settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let config = read_settings(&state).await?;
    let mut value =
        serde_json::to_value(&config).map_err(|error| ApiError::Internal(error.to_string()))?;
    value["readiness"] = readiness(&config).await;
    Ok(Json(value))
}
pub async fn update_settings(
    admin: AdminUser,
    State(state): State<AppState>,
    Json(mut config): Json<Settings>,
) -> Result<Json<Value>, ApiError> {
    config.validate()?; // Readiness is advisory and never rejects a saved choice.
    config.language = normalized_language(&config.language);
    crate::subtitle_models::config::activate_advanced(state.store.as_ref(), config).await?;
    settings(admin, State(state)).await
}

struct Model {
    handle: std::fs::File,
    digest: String,
    identity: plurx_core::fs_secure::FileIdentity,
    english_only: bool,
    languages: Vec<String>,
}
async fn open_model(path: &str) -> Result<Model, StoreError> {
    let path = PathBuf::from(path);
    let handle = tokio::task::spawn_blocking(move || {
        plurx_core::fs_secure::open_read_nofollow_blocking(&path)
    })
    .await
    .map_err(|_| StoreError::Task("model open task failed".into()))?
    .map_err(|_| StoreError::Task("transcription model unavailable".into()))?;
    let meta = handle
        .metadata()
        .map_err(|_| StoreError::Task("model metadata unavailable".into()))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_MODEL_BYTES {
        return Err(StoreError::Task("transcription model exceeds bound".into()));
    }
    let identity = plurx_core::fs_secure::std_file_identity(&handle)
        .map_err(|_| StoreError::Task("model identity unavailable".into()))?;
    let mut reader = tokio::fs::File::from_std(
        handle
            .try_clone()
            .map_err(|error| StoreError::Task(error.to_string()))?,
    );
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut bytes = 0_u64;
    let mut english_only = false;
    let mut languages = vec![];
    loop {
        plurx_core::process::bounded::check_cancellation()
            .map_err(|_| StoreError::Task("model hashing cancelled".into()))?;
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| StoreError::Task(error.to_string()))?;
        if read == 0 {
            break;
        }
        if bytes == 0 && read >= 8 {
            // Standard whisper.cpp GGML header: English-only models have
            // 51864 vocabulary entries; multilingual models add language tokens.
            if u32::from_le_bytes(buffer[..4].try_into().expect("header width")) == 0x67676d6c {
                let vocabulary = u32::from_le_bytes(buffer[4..8].try_into().expect("header width"));
                english_only = vocabulary == 51864;
                languages = languages_for_vocabulary(vocabulary);
            }
        }
        bytes += read as u64;
        if bytes > MAX_MODEL_BYTES {
            return Err(StoreError::Task("model changed or exceeds bound".into()));
        }
        hash.update(&buffer[..read]);
    }
    if bytes != meta.len()
        || plurx_core::fs_secure::std_file_identity(&handle)
            .ok()
            .as_ref()
            != Some(&identity)
    {
        return Err(StoreError::Task("model changed while hashing".into()));
    }
    use std::io::{Seek, SeekFrom};
    (&handle)
        .seek(SeekFrom::Start(0))
        .map_err(|error| StoreError::Task(error.to_string()))?;
    Ok(Model {
        handle,
        identity,
        digest: hex::encode(hash.finalize()),
        english_only,
        languages,
    })
}
fn pipeline_digest(config: &Settings, command: &Path, model: &Model, audio_index: i64) -> String {
    hex::encode(Sha256::digest(format!(
        "whisper.cpp-v3:audio-language-default-lowest-ordinal-v1:copyts-start-at-zero:{PCM_CLOCK_FILTER}:decode-xerror-explode-stderr-empty:mono16k:threads2:{}:{}:{}:{}",
        command.display(),
        normalized_language(&config.language),
        model.digest,
        audio_index
    )))
}
pub(crate) fn eligible(file: &MediaFile) -> bool {
    file.probed
        && file.size > 0
        && !file.audio_streams.is_empty()
        && file
            .duration_ms
            .is_some_and(|duration| duration > 0 && duration <= MAX_DURATION_MS)
}
pub(crate) fn normalized_language(value: &str) -> String {
    let lowered = value.trim().to_ascii_lowercase();
    let primary = lowered.split('-').next().unwrap_or("");
    match primary {
        "eng" => "en",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "jpn" => "ja",
        "zho" | "chi" => "zh",
        "kor" => "ko",
        "rus" => "ru",
        "nld" | "dut" => "nl",
        "ara" => "ar",
        "hin" => "hi",
        "pol" => "pl",
        "tur" => "tr",
        "ukr" => "uk",
        "swe" => "sv",
        "dan" => "da",
        "nor" => "no",
        "fin" => "fi",
        "ces" | "cze" => "cs",
        "ell" | "gre" => "el",
        "ron" | "rum" => "ro",
        "hun" => "hu",
        "heb" => "he",
        "ind" => "id",
        "tha" => "th",
        "vie" => "vi",
        "fil" | "tgl" => "tl",
        "msa" | "may" => "ms",
        "cat" => "ca",
        "slk" | "slo" => "sk",
        "hrv" => "hr",
        "srp" => "sr",
        "bul" => "bg",
        "slv" => "sl",
        "est" => "et",
        "lav" => "lv",
        "lit" => "lt",
        "fas" | "per" => "fa",
        "urd" => "ur",
        "ben" => "bn",
        "tam" => "ta",
        "tel" => "te",
        "mal" => "ml",
        "mar" => "mr",
        "guj" => "gu",
        "pan" => "pa",
        other => other,
    }
    .to_owned()
}
fn language_matches(existing: &str, language: &str) -> bool {
    normalized_language(existing) == normalized_language(language)
}
/// Exact spoken-language match first; default wins within a class, then the
/// smallest actual audio ordinal. An absent/undefined tag is a fallback, never
/// permission to relabel a known incompatible spoken-language track.
fn selected_audio(tracks: &[plurx_core::domain::AudioStream], language: &str) -> Option<i64> {
    let best = |unknown: bool| {
        tracks
            .iter()
            .filter(|track| {
                if track.index < 0 {
                    return false;
                }
                let tag = track.language.as_deref().unwrap_or("").trim();
                let untagged = tag.is_empty()
                    || ["und", "unknown", "n/a"]
                        .iter()
                        .any(|value| tag.eq_ignore_ascii_case(value));
                if unknown {
                    untagged
                } else {
                    !untagged && language_matches(tag, language)
                }
            })
            .min_by_key(|track| (!track.default, track.index))
            .map(|track| track.index)
    };
    best(false).or_else(|| best(true))
}
fn missing_language(file: &MediaFile, language: &str) -> bool {
    !file.subtitle_streams.iter().any(|track| {
        track
            .language
            .as_deref()
            .is_some_and(|existing| language_matches(existing, language))
    })
}
fn enqueue_request(
    file: &MediaFile,
    config: &Settings,
    model: &Model,
    command: &Path,
) -> Result<EnqueueJob, StoreError> {
    let audio_index = selected_audio(&file.audio_streams, &config.language)
        .ok_or_else(|| StoreError::Task("no matching spoken-language audio track".into()))?;
    enqueue_request_selected(file, config, model, command, audio_index)
}
fn enqueue_request_selected(
    file: &MediaFile,
    config: &Settings,
    model: &Model,
    command: &Path,
    audio_index: i64,
) -> Result<EnqueueJob, StoreError> {
    let payload = JobPayload::SubtitleTranscribe {
        file_id: file.id,
        audio_index,
        source_size: file.size,
        source_mtime: file.mtime,
        language: normalized_language(&config.language),
        model_sha256: model.digest.clone(),
        pipeline_digest: pipeline_digest(config, command, model, audio_index),
    };
    let key = artifact_key(&payload)?;
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
            consumer_kind: "subtitle_transcribe".into(),
            consumer_ref: key,
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    })
}
pub async fn enqueue(
    _admin: AdminUser,
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> Result<Json<Value>, ApiError> {
    let config = read_settings(&state).await?;
    config.validate()?;
    if !config.enabled {
        return Err(ApiError::BadRequest(
            "Enable offline subtitle transcription in Developer settings first".into(),
        ));
    }
    let command = executable(&config.command).ok_or_else(|| {
        ApiError::ServiceUnavailable("Local whisper-cli command unavailable".into())
    })?;
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    if !eligible(&file) {
        return Err(ApiError::BadRequest(
            "Transcription requires probed finite media with audio and duration up to four hours"
                .into(),
        ));
    }
    if selected_audio(&file.audio_streams, &config.language).is_none() {
        return Err(ApiError::BadRequest(format!("No audio track matches subtitle language {}; all audio tracks have incompatible language tags", config.language)));
    }
    let model = tokio::time::timeout(Duration::from_secs(60), open_model(&config.model_path))
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Model fingerprint timed out".into()))?
        .map_err(|error| ApiError::ServiceUnavailable(error.to_string()))?;
    if model.english_only && normalized_language(&config.language) != "en" {
        return Err(ApiError::ServiceUnavailable(
            "Configured model supports English only; choose a multilingual model for this language"
                .into(),
        ));
    }
    let outcome = state
        .store
        .enqueue_job(enqueue_request(&file, &config, &model, &command)?)
        .await?;
    Ok(Json(json!(outcome)))
}

fn accepted_audio(file: &MediaFile, audio_index: i64, language: &str) -> bool {
    if audio_index < 0 {
        return false;
    }
    file.audio_streams
        .iter()
        .find(|track| track.index == audio_index)
        .is_some_and(|track| {
            crate::subtitle_models::spoken_language_tag(track.language.as_deref())
                .is_none_or(|tag| normalized_language(&tag) == normalized_language(language))
        })
}

/// Versioned requests freeze the administrator's explicit audio/language choice.
pub(crate) async fn enqueue_advanced_selected(
    state: &AppState,
    file: &MediaFile,
    audio_index: i64,
    language: &str,
) -> Result<(EnqueueOutcome, String), ApiError> {
    let mut config = read_settings(state).await?;
    config.language = normalized_language(language);
    config.validate()?;
    if !config.enabled || !eligible(file) || !accepted_audio(file, audio_index, &config.language) {
        return Err(ApiError::BadRequest("advanced_request_invalid".into()));
    }
    let command = executable(&config.command)
        .ok_or_else(|| ApiError::ServiceUnavailable("engine_unavailable".into()))?;
    let model = tokio::time::timeout(Duration::from_secs(60), open_model(&config.model_path))
        .await
        .map_err(|_| ApiError::ServiceUnavailable("model_verification_failed".into()))?
        .map_err(|_| ApiError::ServiceUnavailable("model_verification_failed".into()))?;
    if !model_supports_language(&model, &config.language) {
        return Err(ApiError::BadRequest("model_language_unsupported".into()));
    }
    let request = enqueue_request_selected(file, &config, &model, &command, audio_index)?;
    let key = artifact_key(&request.payload)?;
    Ok((state.store.enqueue_job(request).await?, key))
}

#[cfg(test)]
async fn transcribe(
    runtime_cache_dir: &Path,
    file: &MediaFile,
    config: &Settings,
    command: &Path,
    model: &Model,
    cancel: &CancellationToken,
    decoder: &str,
) -> Result<String, StoreError> {
    let audio_index = selected_audio(&file.audio_streams, &config.language)
        .ok_or_else(|| StoreError::Task("no matching spoken-language audio track".into()))?;
    transcribe_selected(
        runtime_cache_dir,
        file,
        config,
        command,
        model,
        cancel,
        decoder,
        audio_index,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn transcribe_selected(
    runtime_cache_dir: &Path,
    file: &MediaFile,
    config: &Settings,
    command: &Path,
    model: &Model,
    cancel: &CancellationToken,
    decoder: &str,
    audio_index: i64,
) -> Result<String, StoreError> {
    if plurx_core::fs_secure::std_file_identity(&model.handle).ok() != Some(model.identity) {
        return Err(StoreError::Task("transcription model changed".into()));
    }
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
        .await
        .map_err(StoreError::Task)?;
    let vtt = pipeline(Pipeline {
        purpose: Purpose::Generation,
        runtime_cache_dir,
        source: &source.handle,
        source_path: &file.path,
        model: &model.handle,
        model_path: Path::new(&config.model_path),
        command,
        engine: None,
        audio_index,
        language: &config.language,
        cancel,
        decoder,
        managed: None,
    })
    .await?;
    if !source.unchanged()
        || plurx_core::fs_secure::std_file_identity(&model.handle).ok() != Some(model.identity)
    {
        return Err(StoreError::Task(
            "transcription source or model changed".into(),
        ));
    }
    crate::fragment_index_cluster::open_source_playback_fence(file, Some(source.object_version()))
        .await
        .map_err(StoreError::Task)?;
    Ok(vtt)
}

struct ManagedAttempt<'a> {
    state: &'a AppState,
    fence: &'a crate::background_jobs::JobFence,
    admission: &'a crate::transcode::FragmentAdmission,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Generation,
    SelfTest,
}
struct Pipeline<'a> {
    purpose: Purpose,
    runtime_cache_dir: &'a Path,
    source: &'a std::fs::File,
    #[cfg_attr(not(windows), allow(dead_code))]
    source_path: &'a Path,
    model: &'a std::fs::File,
    #[cfg_attr(not(windows), allow(dead_code))]
    model_path: &'a Path,
    command: &'a Path,
    engine: Option<&'a crate::subtitle_models::artifacts::Engine>,
    audio_index: i64,
    language: &'a str,
    cancel: &'a CancellationToken,
    decoder: &'a str,
    managed: Option<ManagedAttempt<'a>>,
}
impl Pipeline<'_> {
    fn failure(&self, code: &str, legacy: &str) -> StoreError {
        StoreError::Task(if self.engine.is_some() { code } else { legacy }.into())
    }
    fn source_failure(&self) -> StoreError {
        self.failure(
            if self.purpose == Purpose::SelfTest {
                "packaged_engine_companion_changed"
            } else {
                "worker_source_unavailable"
            },
            "transcription source changed",
        )
    }
    fn child_failure(&self, error: std::io::Error, inference: bool) -> StoreError {
        let code = if self.cancel.is_cancelled() || error.kind() == std::io::ErrorKind::Interrupted
        {
            "worker_interrupted"
        } else if inference && error.kind() == std::io::ErrorKind::TimedOut {
            "inference_timeout"
        } else if inference {
            "inference_failed"
        } else {
            "decode_failed"
        };
        self.failure(
            code,
            if inference {
                "transcription command failed or interrupted"
            } else {
                "transcription audio decode failed or interrupted"
            },
        )
    }
    async fn observe(
        &self,
        phase: plurx_core::store::background_jobs_transcription::SubtitlePhase,
    ) -> Result<(), StoreError> {
        if self.cancel.is_cancelled() {
            return Err(StoreError::Task("worker_interrupted".into()));
        }
        if let Some(attempt) = &self.managed {
            let available = if self.purpose == Purpose::SelfTest {
                attempt
                    .state
                    .transcode
                    .speech_capacity_available(attempt.admission)
            } else {
                attempt
                    .state
                    .transcode
                    .fragment_worker_idle(attempt.admission)
            };
            if !available || !attempt.fence.observe_subtitle(phase).await? {
                return Err(StoreError::Task("worker_interrupted".into()));
            }
        }
        Ok(())
    }
}
pub(crate) fn private_attempt(root: &Path) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("attempt-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(root)
}

pub(crate) struct EngineInvocation {
    pub(crate) program: PathBuf,
    staged: Option<(std::fs::File, plurx_core::fs_secure::FileIdentity)>,
}

/// Inspect the decoder's actual PCM data, never infer silence from WAV headers.
/// Only digital zero throughout a nonempty mono16k/s16 stream is refused.
fn pcm_has_signal(
    file: &mut std::fs::File,
    cancel: &CancellationToken,
) -> Result<(bool, u64), &'static str> {
    pcm_has_signal_observed(file, cancel, || {})
}

fn pcm_has_signal_observed(
    file: &mut std::fs::File,
    cancel: &CancellationToken,
    mut after_read: impl FnMut(),
) -> Result<(bool, u64), &'static str> {
    use std::io::{Read, Seek, SeekFrom};
    let interrupted = || {
        if cancel.is_cancelled() {
            Err("worker_interrupted")
        } else {
            Ok(())
        }
    };
    interrupted()?;
    let size = file.metadata().map_err(|_| "decode_failed")?.len();
    if !(46..=MAX_WAV_BYTES).contains(&size) {
        return Err("decode_failed");
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| "decode_failed")?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header).map_err(|_| "decode_failed")?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WAVE"
        || u64::from(u32::from_le_bytes(
            header[4..8].try_into().map_err(|_| "decode_failed")?,
        )) + 8
            != size
    {
        return Err("decode_failed");
    }
    let mut position = 12_u64;
    let mut format_seen = false;
    let mut data_seen = false;
    let mut sample_count = 0;
    let mut signal = false;
    let mut buffer = [0_u8; 128 * 1024];
    for _ in 0..1024 {
        interrupted()?;
        if position == size {
            return if format_seen && data_seen {
                Ok((signal, sample_count))
            } else {
                Err("decode_failed")
            };
        }
        if size - position < 8 {
            return Err("decode_failed");
        }
        let mut chunk = [0_u8; 8];
        file.read_exact(&mut chunk).map_err(|_| "decode_failed")?;
        let length = u64::from(u32::from_le_bytes(
            chunk[4..].try_into().map_err(|_| "decode_failed")?,
        ));
        let end = position
            .checked_add(8)
            .and_then(|start| start.checked_add(length))
            .ok_or("decode_failed")?;
        let padded_end = end.checked_add(length & 1).ok_or("decode_failed")?;
        if padded_end > size {
            return Err("decode_failed");
        }
        match &chunk[..4] {
            b"fmt " => {
                if format_seen || !(16..=4096).contains(&length) {
                    return Err("decode_failed");
                }
                let mut format = [0_u8; 16];
                file.read_exact(&mut format).map_err(|_| "decode_failed")?;
                if format != [1, 0, 1, 0, 128, 62, 0, 0, 0, 125, 0, 0, 2, 0, 16, 0] {
                    return Err("decode_failed");
                }
                format_seen = true;
            }
            b"data" => {
                if !format_seen || data_seen || length == 0 || length % 2 != 0 {
                    return Err("decode_failed");
                }
                data_seen = true;
                sample_count = length / 2;
                let mut remaining = length;
                while remaining > 0 && !signal {
                    interrupted()?;
                    let read = remaining.min(buffer.len() as u64) as usize;
                    file.read_exact(&mut buffer[..read])
                        .map_err(|_| "decode_failed")?;
                    after_read();
                    signal = buffer[..read].iter().any(|byte| *byte != 0);
                    remaining -= read as u64;
                }
            }
            _ => {}
        }
        interrupted()?;
        file.seek(SeekFrom::Start(padded_end))
            .map_err(|_| "decode_failed")?;
        position = padded_end;
    }
    Err("decode_failed")
}

async fn decoded_pcm(path: &Path, cancel: &CancellationToken) -> Result<(bool, u64), &'static str> {
    let mut file = plurx_core::fs_secure::open_read_nofollow(path)
        .await
        .map_err(|_| "decode_failed")?
        .into_std()
        .await;
    let cancel = cancel.clone();
    // Await the owned read loop even on cancellation. It observes the token
    // between bounded chunks and cannot continue detached after scratch retires.
    tokio::task::spawn_blocking(move || pcm_has_signal(&mut file, &cancel))
        .await
        .map_err(|_| "decode_failed")?
}
impl EngineInvocation {
    pub(crate) fn unchanged(&self) -> bool {
        self.staged.as_ref().is_none_or(|(file, identity)| {
            plurx_core::fs_secure::std_file_identity(file).ok() == Some(*identity)
        })
    }
}

/// Darwin refuses executable `/dev/fd/N` handoffs. Execute a verified private
/// copy there, retaining both handles until the owned child has been reaped.
/// Linux executes the original held descriptor directly.
pub(crate) async fn engine_invocation(
    engine: &crate::subtitle_models::artifacts::Engine,
    scratch: &Path,
    cancel: &CancellationToken,
) -> Result<EngineInvocation, StoreError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (engine, scratch, cancel);
        Ok(EngineInvocation {
            program: PathBuf::from("/dev/fd/7"),
            staged: None,
        })
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::MetadataExt,
        };
        let directory = plurx_core::fs_secure::SecureDirectory::open(scratch)
            .await
            .map_err(|_| StoreError::Task("engine_staging_failed".into()))?;
        let path = scratch.join("whisper-cli");
        let source = engine
            .handle
            .try_clone()
            .map_err(|_| StoreError::Task("packaged_engine_changed".into()))?;
        let expected_identity = engine.identity;
        let expected_digest = engine.binary_sha256.clone();
        let cancel = cancel.clone();
        let (staged, identity) = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom, Write};
            let metadata = directory_metadata(&directory)?;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err("engine_staging_failed");
            }
            if expected_identity.size == 0
                || expected_identity.size > 64 * 1024 * 1024
                || plurx_core::fs_secure::std_file_identity(&source).ok() != Some(expected_identity)
            {
                return Err("packaged_engine_changed");
            }
            // SAFETY: fixed single-component name beneath a held private
            // directory; exclusive creation and NOFOLLOW reject substitution.
            let fd = unsafe {
                libc::openat(
                    directory.raw_fd(),
                    c"whisper-cli".as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err("engine_staging_failed");
            }
            let mut output = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut input = &source;
            input
                .seek(SeekFrom::Start(0))
                .map_err(|_| "packaged_engine_changed")?;
            let mut bytes = 0_u64;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 128 * 1024];
            loop {
                if cancel.is_cancelled() {
                    return Err("worker_interrupted");
                }
                let read = input
                    .read(&mut buffer)
                    .map_err(|_| "packaged_engine_changed")?;
                if read == 0 {
                    break;
                }
                bytes += read as u64;
                if bytes > expected_identity.size {
                    return Err("packaged_engine_changed");
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|_| "engine_staging_failed")?;
                digest.update(&buffer[..read]);
            }
            if bytes != expected_identity.size
                || hex::encode(digest.finalize()) != expected_digest
                || plurx_core::fs_secure::std_file_identity(&source).ok() != Some(expected_identity)
            {
                return Err("packaged_engine_changed");
            }
            output.sync_all().map_err(|_| "engine_staging_failed")?;
            output
                .seek(SeekFrom::Start(0))
                .map_err(|_| "engine_staging_failed")?;
            let mut copied = Sha256::new();
            let mut copied_bytes = 0_u64;
            loop {
                if cancel.is_cancelled() {
                    return Err("worker_interrupted");
                }
                let read = output
                    .read(&mut buffer)
                    .map_err(|_| "engine_staging_failed")?;
                if read == 0 {
                    break;
                }
                copied_bytes += read as u64;
                if copied_bytes > expected_identity.size {
                    return Err("engine_staging_failed");
                }
                copied.update(&buffer[..read]);
            }
            if copied_bytes != expected_identity.size
                || hex::encode(copied.finalize()) != expected_digest
            {
                return Err("engine_staging_failed");
            }
            // SAFETY: held exclusively created regular file; mode is owner-only.
            if unsafe { libc::fchmod(output.as_raw_fd(), 0o500) } != 0 {
                return Err("engine_staging_failed");
            }
            let identity = plurx_core::fs_secure::std_file_identity(&output)
                .map_err(|_| "engine_staging_failed")?;
            drop(output);
            // Execution must not retain an open-for-writing description.
            // Reopen only beneath the held private directory capability.
            let fd = unsafe {
                libc::openat(
                    directory.raw_fd(),
                    c"whisper-cli".as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err("engine_staging_failed");
            }
            let readonly = unsafe { std::fs::File::from_raw_fd(fd) };
            if plurx_core::fs_secure::std_file_identity(&readonly).ok() != Some(identity) {
                return Err("engine_staging_failed");
            }
            Ok((readonly, identity))
        })
        .await
        .map_err(|_| StoreError::Task("engine_staging_failed".into()))?
        .map_err(|code| StoreError::Task(code.into()))?;
        // Verify the launch pathname still names the exact staged inode. Its
        // parent is this attempt's private 0700 directory, not the package path.
        let reopened = plurx_core::fs_secure::open_read_nofollow(&path)
            .await
            .map_err(|_| StoreError::Task("engine_staging_failed".into()))?
            .into_std()
            .await;
        if plurx_core::fs_secure::std_file_identity(&reopened).ok() != Some(identity)
            || plurx_core::fs_secure::std_file_identity(&engine.handle).ok()
                != Some(engine.identity)
        {
            return Err(StoreError::Task("packaged_engine_changed".into()));
        }
        Ok(EngineInvocation {
            program: path,
            staged: Some((staged, identity)),
        })
    }
}

#[cfg(target_os = "macos")]
fn directory_metadata(
    directory: &plurx_core::fs_secure::SecureDirectory,
) -> Result<std::fs::Metadata, &'static str> {
    use std::os::fd::FromRawFd;
    // SAFETY: duplicate the live directory descriptor to inspect permissions;
    // ownership of the original capability is unchanged.
    let duplicate = unsafe { libc::fcntl(directory.raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        return Err("engine_staging_failed");
    }
    unsafe { std::fs::File::from_raw_fd(duplicate) }
        .metadata()
        .map_err(|_| "engine_staging_failed")
}

async fn pipeline(input: Pipeline<'_>) -> Result<String, StoreError> {
    use plurx_core::store::background_jobs_transcription::SubtitlePhase;
    let Pipeline {
        runtime_cache_dir,
        source,
        source_path: _,
        model,
        model_path: _,
        command,
        audio_index,
        language,
        cancel,
        decoder,
        ..
    } = &input;
    input.observe(SubtitlePhase::Decoding).await?;
    let source_identity =
        plurx_core::fs_secure::std_file_identity(source).map_err(|_| input.source_failure())?;
    let model_identity = plurx_core::fs_secure::std_file_identity(model).map_err(|_| {
        input.failure(
            "artifact_digest_mismatch",
            "transcription model unavailable",
        )
    })?;
    use std::io::{Seek, SeekFrom};
    let mut source_reader = *source;
    source_reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| input.source_failure())?;
    let audio_map = format!("0:a:{audio_index}");
    let scratch_root = runtime_cache_dir.join("subtitle-transcription");
    // Reserve the bounded WAV plus engine output before the private attempt
    // can write. The reservation outlives TempDir cleanup on every exit.
    let _scratch_reservation = crate::playback_cache::reserve(
        &scratch_root,
        MAX_WAV_BYTES.saturating_add(80 * 1024 * 1024),
    )
    .await
    .map_err(|_| input.failure("output_invalid", "transcription disk budget exhausted"))?;
    tokio::fs::create_dir_all(&scratch_root)
        .await
        .map_err(|_| {
            input.failure(
                "output_invalid",
                "transcription filesystem operation failed",
            )
        })?;
    let scratch = private_attempt(&scratch_root).map_err(|_| {
        input.failure(
            "output_invalid",
            "transcription filesystem operation failed",
        )
    })?;
    let wav = scratch.path().join("audio.wav");
    let decode_threads = if input.purpose == Purpose::SelfTest {
        "1"
    } else {
        "2"
    };
    let mut decode = tokio::process::Command::new(decoder);
    if input.purpose == Purpose::SelfTest {
        decode.args(["-filter_threads", "1"]);
    }
    decode.args([
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-err_detect",
        "explode",
        "-y",
        "-copyts",
        "-start_at_zero",
        "-protocol_whitelist",
        "file,pipe",
        "-threads",
        decode_threads,
        "-i",
    ]);
    #[cfg(unix)]
    {
        decode.arg("/dev/fd/3");
        crate::ffmpeg::inherit_file_descriptors(&mut decode, &[(source, 3)]);
    }
    #[cfg(windows)]
    {
        crate::ffmpeg::verify_windows_source_path(source, input.source_path)
            .map_err(StoreError::Task)?;
        decode.arg(input.source_path);
    }
    decode
        .args([
            "-map",
            &audio_map,
            "-vn",
            "-sn",
            "-dn",
            "-af",
            PCM_CLOCK_FILTER,
            "-threads",
            decode_threads,
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-fs",
            &(MAX_WAV_BYTES + 1).to_string(),
        ])
        .arg(&wav);
    crate::subtitle_ride_along::configure_stage_file_limit(&mut decode, MAX_WAV_BYTES + 1)
        .map_err(|error| {
            input.failure(
                "output_invalid",
                &format!("bounded transcription output unavailable: {error}"),
            )
        })?;
    let output = pipeline_output(
        &input,
        &mut decode,
        if input.purpose == Purpose::SelfTest {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(30 * 60)
        },
        256 * 1024,
        plurx_core::process::ChildWork::background("subtitle transcription audio"),
    )
    .await
    .map_err(|error| input.child_failure(error, false))?;
    // Under -v error any diagnostic is an error, even if a demuxer/decoder
    // tolerates partial corruption and exits zero. Never let that partial
    // waveform become complete captions that suppress later discovery.
    if !output.status.success() || output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(input.failure("decode_failed", "transcription audio decode failed"));
    }
    let meta = tokio::fs::symlink_metadata(&wav).await.map_err(|_| {
        input.failure(
            "output_invalid",
            "transcription filesystem operation failed",
        )
    })?;
    if !meta.is_file() || meta.len() <= 44 || meta.len() > MAX_WAV_BYTES {
        return Err(input.failure(
            "decode_failed",
            "transcription audio exceeds four-hour bound",
        ));
    }
    let sample_count = if input.engine.is_some() {
        Some(match decoded_pcm(&wav, cancel).await {
            Ok((true, sample_count)) => sample_count,
            Ok((false, _)) => return Err(StoreError::Task("no_speech_detected".into())),
            Err("worker_interrupted") => return Err(StoreError::Task("worker_interrupted".into())),
            Err(_) => {
                return Err(input.failure("decode_failed", "transcription audio decode failed"))
            }
        })
    } else {
        None
    };
    // The model fingerprint is held across attempts; reset the shared open
    // description before passing its descriptor to a new child.
    let mut model_reader = *model;
    model_reader.seek(SeekFrom::Start(0)).map_err(|_| {
        input.failure(
            "output_invalid",
            "transcription filesystem operation failed",
        )
    })?;
    input.observe(SubtitlePhase::Transcribing).await?;
    let output_base = scratch.path().join("caption");
    let invocation = if let Some(engine) = input.engine {
        Some(engine_invocation(engine, scratch.path(), input.cancel).await?)
    } else {
        None
    };
    let mut whisper = tokio::process::Command::new(
        invocation
            .as_ref()
            .map_or(*command, |invocation| invocation.program.as_path()),
    );
    let whisper_language = normalized_language(language);
    whisper.args([
        "--threads",
        "2",
        "--processors",
        "1",
        "--no-gpu",
        "--language",
        &whisper_language,
        "--output-vtt",
        "--no-prints",
        "--model",
    ]);
    #[cfg(unix)]
    {
        whisper.arg("/dev/fd/5");
        if let Some(engine) = input.engine {
            crate::ffmpeg::inherit_file_descriptors(
                &mut whisper,
                &[(model, 5), (&engine.handle, 7)],
            );
        } else {
            crate::ffmpeg::inherit_file_descriptors(&mut whisper, &[(model, 5)]);
        }
    }
    #[cfg(windows)]
    {
        crate::ffmpeg::verify_windows_source_path(model, input.model_path)
            .map_err(StoreError::Task)?;
        whisper.arg(input.model_path);
    }
    whisper
        .arg("--file")
        .arg(&wav)
        .arg("--output-file")
        .arg(&output_base);
    crate::subtitle_ride_along::configure_stage_file_limit(
        &mut whisper,
        (MAX_DOWNLOADED_SUBTITLE_BYTES + 1) as u64,
    )
    .map_err(|error| {
        input.failure(
            "output_invalid",
            &format!("bounded transcription output unavailable: {error}"),
        )
    })?;
    let output = pipeline_output(
        &input,
        &mut whisper,
        if input.purpose == Purpose::SelfTest {
            Duration::from_secs(120)
        } else {
            WALL
        },
        256 * 1024,
        plurx_core::process::ChildWork::background("subtitle transcription whisper.cpp"),
    )
    .await
    .map_err(|error| input.child_failure(error, true))?;
    if cancel.is_cancelled() {
        return Err(input.failure(
            "worker_interrupted",
            "transcription command failed or interrupted",
        ));
    }
    if !output.status.success() {
        return Err(input.failure(
            "inference_failed",
            "transcription command failed or interrupted",
        ));
    }
    input.observe(SubtitlePhase::Validating).await?;
    if invocation
        .as_ref()
        .is_some_and(|invocation| !invocation.unchanged())
    {
        return Err(StoreError::Task("packaged_engine_changed".into()));
    }
    if input.engine.is_some_and(|engine| {
        plurx_core::fs_secure::std_file_identity(&engine.handle).ok() != Some(engine.identity)
    }) {
        return Err(StoreError::Task("packaged_engine_changed".into()));
    }
    if plurx_core::fs_secure::std_file_identity(model).ok() != Some(model_identity) {
        return Err(StoreError::Task(if input.engine.is_some() {
            "artifact_digest_mismatch".into()
        } else {
            "transcription model changed".into()
        }));
    }
    if plurx_core::fs_secure::std_file_identity(source).ok() != Some(source_identity) {
        return Err(input.source_failure());
    }
    let path = output_base.with_extension("vtt");
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| input.failure("output_invalid", "transcription produced no captions"))?;
    if !meta.is_file() {
        return Err(input.failure("output_invalid", "transcription output is not regular"));
    }
    if meta.len() > MAX_DOWNLOADED_SUBTITLE_BYTES as u64 {
        return Err(input.failure(
            "output_too_large",
            "transcription caption output exceeds bound",
        ));
    }
    let file = tokio::fs::File::open(&path).await.map_err(|_| {
        input.failure(
            "output_invalid",
            "transcription filesystem operation failed",
        )
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_DOWNLOADED_SUBTITLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| {
            input.failure(
                "output_invalid",
                "transcription filesystem operation failed",
            )
        })?;
    if bytes.len() > MAX_DOWNLOADED_SUBTITLE_BYTES {
        return Err(input.failure(
            "output_too_large",
            "transcription caption output exceeds bound",
        ));
    }
    let vtt = String::from_utf8(bytes)
        .map_err(|_| input.failure("output_invalid", "transcription captions are not UTF-8"))?
        .replace("\r\n", "\n");
    if !complete_caption(&vtt) {
        return Err(input.failure(
            "output_invalid",
            "transcription did not produce complete valid captions",
        ));
    }
    if let Some(sample_count) = sample_count {
        let normalized = intersect_caption_with_pcm(&vtt, sample_count).ok_or_else(|| {
            input.failure(
                "output_invalid",
                "transcription captions exceed the media timeline",
            )
        })?;
        tracing::debug!(raw_vtt_sha256 = %hex::encode(Sha256::digest(vtt.as_bytes())), clipped = normalized.clipped, discarded = normalized.discarded, "managed subtitle PCM timeline intersection");
        Ok(normalized.vtt)
    } else {
        Ok(vtt)
    }
}

async fn pipeline_output(
    input: &Pipeline<'_>,
    command: &mut tokio::process::Command,
    wall: Duration,
    limit: usize,
    work: plurx_core::process::ChildWork,
) -> std::io::Result<plurx_core::process::bounded::Output> {
    if input.engine.is_some() {
        plurx_core::process::bounded::output_command_clean_env(command, wall, limit, work).await
    } else {
        plurx_core::process::bounded::output_command(command, wall, limit, work).await
    }
}

/// Header-only capability discovery for an explicitly configured Advanced model.
/// Unknown formats cannot advertise a language merely because a file exists.
pub(crate) async fn model_languages(path: &str) -> Vec<String> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let Ok(mut file) = plurx_core::fs_secure::open_read_nofollow_blocking(&path) else {
            return vec![];
        };
        let Ok(identity) = plurx_core::fs_secure::std_file_identity(&file) else {
            return vec![];
        };
        let mut header = [0_u8; 8];
        if file.read_exact(&mut header).is_err()
            || identity.size > MAX_MODEL_BYTES
            || plurx_core::fs_secure::std_file_identity(&file).ok() != Some(identity)
            || u32::from_le_bytes(header[..4].try_into().expect("header width")) != 0x67676d6c
        {
            return vec![];
        }
        languages_for_vocabulary(u32::from_le_bytes(
            header[4..].try_into().expect("header width"),
        ))
    })
    .await
    .unwrap_or_default()
}

fn languages_for_vocabulary(vocabulary: u32) -> Vec<String> {
    match vocabulary {
        51864 => vec!["en".into()],
        // Pinned whisper.cpp 306c88f4 src/whisper.cpp::whisper_vocab derives
        // n_languages = n_vocab - 51766: 51865 has 99; 51866 adds yue (ID99).
        // This is Advanced header capability, not managed model qualification.
        51865 | 51866 => plurx_core::store::background_jobs_transcription::WHISPER_LANGUAGES
            .iter()
            .filter(|language| vocabulary == 51866 || **language != "yue")
            .map(|language| (*language).into())
            .collect(),
        _ => vec![],
    }
}

fn model_supports_language(model: &Model, language: &str) -> bool {
    model.languages.contains(&normalized_language(language))
}

async fn managed_pipeline(input: Pipeline<'_>) -> Result<String, StoreError> {
    let self_test = input.purpose == Purpose::SelfTest;
    let cancel = input.cancel.clone();
    let attempt = input.managed.as_ref().expect("managed attempt");
    let state = attempt.state;
    let admission = attempt.admission;
    let operation = plurx_core::process::bounded::cancellable(cancel.clone(), pipeline(input));
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = watch_capacity_until(|| if self_test { state.transcode.speech_capacity_available(admission) } else { state.transcode.fragment_worker_idle(admission) }) => { cancel.cancel(); operation.await }
    }
}

pub(crate) fn managed_artifacts_unchanged(
    engine: &crate::subtitle_models::artifacts::Engine,
    model: &crate::subtitle_models::artifacts::ManagedModel,
) -> Result<(), StoreError> {
    if plurx_core::fs_secure::std_file_identity(&engine.handle).ok() != Some(engine.identity) {
        return Err(StoreError::Task("packaged_engine_changed".into()));
    }
    if plurx_core::fs_secure::std_file_identity(&model.handle).ok() != Some(model.identity) {
        return Err(StoreError::Task("artifact_digest_mismatch".into()));
    }
    Ok(())
}

async fn managed_source(
    file: &MediaFile,
    expected: Option<&str>,
) -> Result<crate::fragment_index_cluster::SourceFence, StoreError> {
    crate::fragment_index_cluster::open_source_playback_fence(file, expected)
        .await
        .map_err(|_| StoreError::Task("worker_source_unavailable".into()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn transcribe_managed(
    state: &AppState,
    file: &MediaFile,
    payload: &JobPayload,
    engine: &crate::subtitle_models::artifacts::Engine,
    model: &crate::subtitle_models::artifacts::ManagedModel,
    fence: &crate::background_jobs::JobFence,
    admission: &crate::transcode::FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<DownloadedSubtitle, StoreError> {
    let JobPayload::SubtitleTranscribeManaged {
        file_id,
        audio_index,
        source_size,
        source_mtime,
        language,
        model_sha256,
        pipeline_digest,
        profile,
    } = payload
    else {
        return Err(StoreError::Task("managed_payload_invalid".into()));
    };
    payload.validate()?;
    if !eligible(file)
        || file.id != *file_id
        || file.size != *source_size
        || file.mtime != *source_mtime
        || !file
            .audio_streams
            .iter()
            .any(|track| track.index == *audio_index)
    {
        return Err(StoreError::Task("source_changed".into()));
    }
    if *model_sha256 != model.sha256 {
        return Err(StoreError::Task("artifact_digest_mismatch".into()));
    }
    if profile.adapter_contract != engine.adapter_contract {
        return Err(StoreError::Task("packaged_engine_unqualified".into()));
    }
    managed_artifacts_unchanged(engine, model)?;
    let source = managed_source(file, None).await?;
    let cancel = manager_cancel.child_token();
    let decoder = crate::ffmpeg::ffmpeg_bin();
    let cache = scratch_cache(state);
    let vtt = managed_pipeline(Pipeline {
        purpose: Purpose::Generation,
        runtime_cache_dir: &cache,
        source: &source.handle,
        source_path: &file.path,
        model: &model.handle,
        model_path: &model.path,
        command: &engine.path,
        engine: Some(engine),
        audio_index: *audio_index,
        language,
        cancel: &cancel,
        decoder: &decoder,
        managed: Some(ManagedAttempt {
            state,
            fence,
            admission,
        }),
    })
    .await?;
    managed_artifacts_unchanged(engine, model)?;
    if cancel.is_cancelled() {
        return Err(StoreError::Task("worker_interrupted".into()));
    }
    if !source.unchanged() {
        return Err(StoreError::Task("worker_source_unavailable".into()));
    }
    managed_source(file, Some(source.object_version())).await?;
    Ok(DownloadedSubtitle {
        text_translation: None,
        source_size: *source_size,
        source_mtime: *source_mtime,
        provider_file_id: 0,
        transcription: Some(SubtitleTranscription {
            audio_index: *audio_index,
            artifact_key: artifact_key(payload)?,
            model_sha256: model.sha256.clone(),
            pipeline_digest: pipeline_digest.clone(),
            adapter: engine.adapter_contract.clone(),
            execution: Some(plurx_core::domain::SubtitleExecution {
                engine_sha256: engine.binary_sha256.clone(),
                platform: engine.platform.clone(),
            }),
            generated_at_ms: crate::state::clock_ms(),
        }),
        language: language.clone(),
        title: "Machine transcription (whisper.cpp)".into(),
        hearing_impaired: false,
        forced: false,
        vtt,
    })
}

pub(crate) async fn self_test_managed(
    state: &AppState,
    engine: &crate::subtitle_models::artifacts::Engine,
    model: &crate::subtitle_models::artifacts::ManagedModel,
    fence: &crate::background_jobs::JobFence,
    admission: &crate::transcode::FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<(), StoreError> {
    managed_artifacts_unchanged(engine, model)?;
    if engine.self_test_sha256
        != hex::encode(Sha256::digest(include_bytes!(
            "subtitle_models/self-test.wav"
        )))
        || plurx_core::fs_secure::std_file_identity(&engine.self_test_handle).ok()
            != Some(engine.self_test_identity)
    {
        return Err(StoreError::Task("packaged_engine_companion_changed".into()));
    }
    use std::io::{Seek, SeekFrom};
    (&engine.self_test_handle)
        .seek(SeekFrom::Start(0))
        .map_err(|_| StoreError::Task("packaged_engine_companion_changed".into()))?;
    let cancel = manager_cancel.child_token();
    let decoder = crate::ffmpeg::ffmpeg_bin();
    let cache = scratch_cache(state);
    let vtt = managed_pipeline(Pipeline {
        purpose: Purpose::SelfTest,
        runtime_cache_dir: &cache,
        source: &engine.self_test_handle,
        source_path: Path::new(""),
        model: &model.handle,
        model_path: &model.path,
        command: &engine.path,
        engine: Some(engine),
        audio_index: 0,
        language: "en",
        cancel: &cancel,
        decoder: &decoder,
        managed: Some(ManagedAttempt {
            state,
            fence,
            admission,
        }),
    })
    .await?;
    managed_artifacts_unchanged(engine, model)?;
    if plurx_core::fs_secure::std_file_identity(&engine.self_test_handle).ok()
        != Some(engine.self_test_identity)
    {
        return Err(StoreError::Task("packaged_engine_companion_changed".into()));
    }
    if cancel.is_cancelled() {
        return Err(StoreError::Task("worker_interrupted".into()));
    }
    if !self_test_caption(&vtt) {
        return Err(StoreError::Task("model_self_test_failed".into()));
    }
    Ok(())
}

fn self_test_caption(vtt: &str) -> bool {
    let words = vtt
        .split(|c: char| !c.is_ascii_alphabetic())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    complete_caption(vtt)
        && ["ask", "country", "you"]
            .iter()
            .all(|word| words.iter().any(|actual| actual == word))
        && vtt
            .lines()
            .filter(|line| line.contains(" --> "))
            .all(|line| {
                line.split(" --> ").take(2).all(|stamp| {
                    let fields = stamp.split([':', '.']).collect::<Vec<_>>();
                    fields.len() == 4
                        && fields
                            .iter()
                            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
                        && fields[0] == "00"
                        && fields[1] == "00"
                        && fields[2]
                            .parse::<u32>()
                            .ok()
                            .zip(fields[3].parse::<u32>().ok())
                            .is_some_and(|(seconds, milliseconds)| {
                                seconds
                                    .checked_mul(1000)
                                    .and_then(|value| value.checked_add(milliseconds))
                                    .is_some_and(|value| value <= 11_000)
                            })
                })
            })
}

struct CaptionIntersection {
    vtt: String,
    clipped: usize,
    discarded: usize,
}

fn caption_timestamp(stamp: &str) -> Option<u64> {
    let fields = stamp.split([':', '.']).collect::<Vec<_>>();
    if fields.len() != 4
        || fields[0].len() < 2
        || fields[1].len() != 2
        || fields[2].len() != 2
        || fields[3].len() != 3
        || fields
            .iter()
            .any(|field| !field.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let hours = fields[0].parse::<u64>().ok()?;
    let minutes = fields[1].parse::<u64>().ok()?;
    let seconds = fields[2].parse::<u64>().ok()?;
    if minutes > 59 || seconds > 59 {
        return None;
    }
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000 + seconds * 1000 + fields[3].parse::<u64>().ok()?)
}

fn caption_stamp(milliseconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        milliseconds / 3_600_000,
        milliseconds / 60_000 % 60,
        milliseconds / 1000 % 60,
        milliseconds % 1000
    )
}

/// Deterministic intersection with decoded media time, not semantic filtering.
fn intersect_caption_with_pcm(vtt: &str, sample_count: u64) -> Option<CaptionIntersection> {
    if !complete_caption(vtt) {
        return None;
    }
    let end_ms = sample_count.checked_mul(1000)?.checked_div(16000)?;
    if end_ms == 0 || end_ms > MAX_DURATION_MS as u64 {
        return None;
    }
    let mut result = CaptionIntersection {
        vtt: "WEBVTT\n\n".into(),
        clipped: 0,
        discarded: 0,
    };
    let mut previous_start = None;
    for block in vtt[8..].trim_matches('\n').split("\n\n") {
        let mut lines = block.lines().collect::<Vec<_>>();
        let timing = usize::from(!lines.first()?.contains(" --> "));
        let (start, end) = lines.get(timing)?.split_once(" --> ")?;
        let start_ms = caption_timestamp(start)?;
        let cue_end = caption_timestamp(end)?;
        if cue_end <= start_ms || previous_start.is_some_and(|previous| start_ms < previous) {
            return None;
        }
        previous_start = Some(start_ms);
        if start_ms >= end_ms {
            result.discarded += 1;
            continue;
        }
        let clipped;
        if cue_end > end_ms {
            clipped = format!("{start} --> {}", caption_stamp(end_ms));
            lines[timing] = &clipped;
            result.clipped += 1;
        }
        result.vtt.push_str(&lines.join("\n"));
        result.vtt.push_str("\n\n");
    }
    complete_caption(&result.vtt).then_some(result)
}

fn complete_caption(vtt: &str) -> bool {
    if !plurx_core::store::valid_downloaded_vtt(vtt)
        || !vtt.starts_with("WEBVTT\n\n")
        || !vtt.ends_with('\n')
    {
        return false;
    }
    vtt[8..].trim().split("\n\n").all(|block| {
        let lines: Vec<_> = block.lines().collect();
        let timing = usize::from(lines.first().is_some_and(|line| !line.contains(" --> ")));
        lines.len() > timing + 1
            && lines[timing].contains(" --> ")
            && plurx_core::store::valid_downloaded_vtt(&format!("WEBVTT\n\n{block}\n"))
    })
}

/// Each node owns its scratch even when operators share a runtime-cache mount.
fn scratch_cache(state: &AppState) -> PathBuf {
    state
        .runtime_cache_dir
        .join("subtitle-transcription-nodes")
        .join(hex::encode(Sha256::digest(state.node_id.as_bytes())))
}

pub(crate) async fn run(state: AppState, shutdown: CancellationToken) {
    let boot = uuid::Uuid::new_v4().to_string();
    let root = scratch_cache(&state).join("subtitle-transcription");
    if let Ok(mut entries) = tokio::fs::read_dir(&root).await {
        for _ in 0..32 {
            let Ok(Some(entry)) = entries.next_entry().await else {
                break;
            };
            if entry.file_name().to_string_lossy().starts_with("attempt-") {
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
        }
    }
    let mut model_cache = None;
    let mut slow_due = tokio::time::Instant::now();
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let translating = match crate::subtitle_translation::worker_pass(&state, &shutdown).await {
            Ok(active) => active,
            Err(error) => {
                tracing::debug!(%error,"translation worker pass stopped");
                false
            }
        };
        let translating_text =
            match crate::subtitle_text_translation::manager::worker_pass(&state, &shutdown).await {
                Ok(active) => active,
                Err(error) => {
                    tracing::debug!(%error,"text subtitle worker pass stopped");
                    false
                }
            };
        if tokio::time::Instant::now() >= slow_due {
            if let Err(error) = crate::subtitle_models::worker_pass(&state, &shutdown).await {
                tracing::debug!(%error, "managed subtitle worker pass stopped");
            }
            if let Err(error) = pass(&state, &boot, &shutdown, &mut model_cache).await {
                tracing::debug!(%error,"offline subtitle transcription pass stopped");
            }
            slow_due = tokio::time::Instant::now() + Duration::from_secs(15);
        }
        // Live translation demand is served each second; otherwise the loop
        // sleeps until the 15 s passes are due or a translation mutation wakes it.
        let idle = if translating || translating_text {
            Duration::from_secs(1)
        } else {
            slow_due.saturating_duration_since(tokio::time::Instant::now())
        };
        tokio::select! { ()=shutdown.cancelled()=>return, ()=tokio::time::sleep(idle)=>{}, ()=state.subtitle_models.translation.wake.notified()=>{} }
    }
}
async fn pass(
    state: &AppState,
    boot: &str,
    shutdown: &CancellationToken,
    model_cache: &mut Option<(String, Model)>,
) -> Result<(), StoreError> {
    let config = read_settings(state).await?;
    if !config.enabled {
        return Ok(());
    }
    let authority = state.jobs.execution_authority();
    if !authority.may_execute_job(JobKind::SubtitleTranscribe).await {
        return Ok(());
    }
    let Some(command) = executable(&config.command) else {
        return Ok(());
    };
    let Some(admission) = state.transcode.admit_fragment().await else {
        return Ok(());
    };
    // Physical capacity remains held through model reads, children and scratch cleanup.
    let path = PathBuf::from(&config.model_path);
    let observed = tokio::task::spawn_blocking(move || {
        let handle = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        plurx_core::fs_secure::std_file_identity(&handle).ok()
    })
    .await
    .ok()
    .flatten();
    if !model_cache
        .as_ref()
        .is_some_and(|(path, model)| path == &config.model_path && Some(model.identity) == observed)
    {
        *model_cache = None;
        let cancel = shutdown.child_token();
        let operation = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            open_model(&config.model_path),
        );
        tokio::pin!(operation);
        let model = tokio::select! {
            result = &mut operation => result?,
            () = shutdown.cancelled() => { cancel.cancel(); let _ = operation.await; return Ok(()); }
        };
        *model_cache = Some((config.model_path.clone(), model));
    }
    let model = &model_cache.as_ref().expect("model fingerprint installed").1;
    if !state.transcode.fragment_worker_idle(&admission) {
        return Ok(());
    }
    if config.interval_mins > 0
        && (!model.english_only || normalized_language(&config.language) == "en")
    {
        if let Some(lease) = state
            .jobs
            .acquire_job("subtitle-transcription:sweep".into())
            .await?
        {
            let result = automatic_page(state, &config, model, &command, &lease).await;
            lease.release().await?;
            result?;
        } // Another discovery owner never prevents this node claiming manual work.
    }
    let page = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![JobKind::SubtitleTranscribe],
            after: None,
            now_ms: crate::state::clock_ms(),
            limit: 32,
        })
        .await?;
    for candidate in page.jobs {
        let Ok(payload @ JobPayload::SubtitleTranscribe { .. }) = candidate.supported_payload()
        else {
            continue;
        };
        let JobPayload::SubtitleTranscribe {
            file_id,
            audio_index,
            source_size,
            source_mtime,
            ref pipeline_digest,
            ref language,
            ..
        } = payload
        else {
            unreachable!()
        };
        let mut accepted_config = config.clone();
        accepted_config.language = language.clone();
        if model.english_only && normalized_language(language) != "en" {
            continue;
        }
        if pipeline_digest != &self::pipeline_digest(&accepted_config, &command, model, audio_index)
        {
            continue;
        }
        let file = state.store.get_file(file_id).await?;
        if let Some(file) = &file {
            if file.size == source_size
                && file.mtime == source_mtime
                && crate::fragment_index_cluster::open_source_playback_fence(file, None)
                    .await
                    .is_err()
            {
                continue;
            }
        }
        if shutdown.is_cancelled() || !state.transcode.fragment_worker_idle(&admission) {
            return Ok(());
        }
        let now = crate::state::clock_ms();
        let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
            state.store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: boot.into(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleTranscribe,
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
            Arc::clone(&state.store),
            Arc::clone(&authority),
            job.token
                .clone()
                .ok_or_else(|| StoreError::Task("transcription claim missing token".into()))?,
            deadline,
            JobKind::SubtitleTranscribe,
        )?;
        let fence = active.fence();
        let Some(file) = file else {
            let _ = fence
                .settle(JobSettlement::Stop {
                    error_code: "transcription_source_missing".into(),
                })
                .await;
            active.finish().await;
            continue;
        };
        let cancel = fence.loss_token().child_token();
        let operation = async {
            if file.size != source_size
                || file.mtime != source_mtime
                || !eligible(&file)
                || !accepted_audio(&file, audio_index, &accepted_config.language)
            {
                return Err(StoreError::Task(
                    "transcription source changed or exceeds bound".into(),
                ));
            }
            plurx_core::process::bounded::cancellable(
                cancel.clone(),
                transcribe_selected(
                    &scratch_cache(state),
                    &file,
                    &accepted_config,
                    &command,
                    model,
                    &cancel,
                    &crate::ffmpeg::ffmpeg_bin(),
                    audio_index,
                ),
            )
            .await
        };
        let result =
            await_claimed_inference(operation, &state.transcode, &admission, shutdown, &cancel)
                .await;
        if cancel.is_cancelled() || shutdown.is_cancelled() {
            let _ = fence
                .settle(JobSettlement::Yield {
                    error_code: Some("worker_interrupted".into()),
                    checkpoint: None,
                    not_before_ms: crate::state::clock_ms() + 5000,
                })
                .await;
        } else {
            match result {
                Ok(vtt) => {
                    let caption = DownloadedSubtitle {
                        text_translation: None,
                        source_size,
                        source_mtime,
                        provider_file_id: 0,
                        transcription: Some(SubtitleTranscription {
                            audio_index,
                            artifact_key: artifact_key(&payload)?,
                            model_sha256: model.digest.clone(),
                            pipeline_digest: self::pipeline_digest(
                                &accepted_config,
                                &command,
                                model,
                                audio_index,
                            ),
                            adapter: "whisper.cpp".into(),
                            execution: None,
                            generated_at_ms: crate::state::clock_ms(),
                        }),
                        language: accepted_config.language.clone(),
                        title: "Machine transcription (whisper.cpp)".into(),
                        hearing_impaired: false,
                        forced: false,
                        vtt,
                    };
                    if !fence.publish_transcription(payload, caption).await? {
                        let _ = fence
                            .settle(JobSettlement::Stop {
                                error_code: "transcription_publication_refused".into(),
                            })
                            .await;
                    }
                }
                Err(error) => {
                    tracing::warn!(job_id = %job.id, %error, "offline subtitle transcription refused");
                    let _ = fence
                        .settle(JobSettlement::Fail {
                            error_code: "subtitle_transcription_failed".into(),
                        })
                        .await;
                }
            }
        }
        active.finish().await;
        return Ok(());
    }
    Ok(())
}
/// A saved disable/mode change stops future claims. An accepted attempt keeps
/// its captured recipe; only Stop, shutdown or foreground demand interrupts it.
async fn await_claimed_inference<T>(
    operation: impl std::future::Future<Output = Result<T, StoreError>>,
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
    shutdown: &CancellationToken,
    cancel: &CancellationToken,
) -> Result<T, StoreError> {
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = shutdown.cancelled() => { cancel.cancel(); operation.await },
        () = watch_capacity(transcode, admission) => { cancel.cancel(); operation.await },
    }
}
pub(crate) async fn watch_capacity(
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
) {
    watch_capacity_until(|| transcode.fragment_worker_idle(admission)).await
}
pub(crate) async fn watch_speech_capacity(
    transcode: &crate::transcode::TranscodeManager,
    admission: &crate::transcode::FragmentAdmission,
) {
    watch_capacity_until(|| transcode.speech_capacity_available(admission)).await
}
async fn watch_capacity_until(available: impl Fn() -> bool) {
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if !available() {
            return;
        }
    }
}

async fn automatic_page(
    state: &AppState,
    config: &Settings,
    model: &Model,
    command: &Path,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<(), StoreError> {
    let now = crate::state::clock_ms();
    let next = state
        .store
        .get_setting(&format!("{PREFIX}next_run"))
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if next > now {
        return Ok(());
    }
    let cursor = state
        .store
        .get_setting(&format!("{PREFIX}cursor"))
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let publisher = lease.publisher(state.store.as_ref());
    publisher
        .put_setting(
            &format!("{PREFIX}next_run"),
            &(now + i64::from(config.interval_mins) * 60_000).to_string(),
        )
        .await?;
    let ids = state.store.subtitle_candidate_file_ids(cursor, 8).await?;
    for id in &ids {
        if !read_settings(state).await?.enabled {
            break;
        }
        if let Some(file) = state.store.get_file(*id).await? {
            if eligible(&file)
                && selected_audio(&file.audio_streams, &config.language).is_some()
                && missing_language(&file, &config.language)
            {
                let _ = publisher
                    .enqueue_transcription(enqueue_request(&file, config, model, command)?)
                    .await?;
            }
        }
        publisher
            .put_setting(&format!("{PREFIX}cursor"), &id.to_string())
            .await?;
    }
    if ids.is_empty() {
        publisher
            .put_setting(&format!("{PREFIX}cursor"), "0")
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pcm_fixture(path: &Path, signal: bool) {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&32036_u32.to_le_bytes());
        bytes.extend_from_slice(
            b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data",
        );
        bytes.extend_from_slice(&32000_u32.to_le_bytes());
        bytes.resize(32044, 0);
        if signal {
            bytes[44] = 1;
        }
        std::fs::write(path, bytes).expect("PCM fixture");
    }

    #[test]
    fn managed_caption_intersection_preserves_text_and_intersects_only_pcm_time() {
        let vtt = "WEBVTT\n\nfirst\n00:00:00.000 --> 00:00:01.000\nExact EOF.\n\nsecond\n00:00:00.500 --> 00:00:02.000\n[BLANK_AUDIO] retained.\n\nthird\n00:00:01.000 --> 00:00:02.000\nWholly outside.\n\n";
        let result = intersect_caption_with_pcm(vtt, 16000).expect("ordered overlaps allowed");
        assert_eq!((result.clipped, result.discarded), (1, 1));
        assert_eq!(result.vtt, "WEBVTT\n\nfirst\n00:00:00.000 --> 00:00:01.000\nExact EOF.\n\nsecond\n00:00:00.500 --> 00:00:01.000\n[BLANK_AUDIO] retained.\n\n");
        for invalid in [
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nOutside.\n\n",
            "WEBVTT\n\n00:00:00.000 --> 00:00:00.000\nZero.\n\n",
            "WEBVTT\n\n00:00:00.500 --> 00:00:01.000\nFirst.\n\n00:00:00.400 --> 00:00:00.900\nDecreasing.\n\n",
            "WEBVTT\n\n00:60:00.000 --> 00:60:01.000\nMalformed.\n\n",
            "WEBVTT\n\n99999999999999999999:00:00.000 --> 99999999999999999999:00:01.000\nOverflow.\n\n",
            "WEBVTT\n\n00:00:00.NaN --> 00:00:01.000\nNot finite.\n\n",
        ] {
            assert!(intersect_caption_with_pcm(invalid, 16000).is_none(), "{invalid}");
        }
        assert!(intersect_caption_with_pcm(vtt, u64::MAX).is_none());
        assert!(intersect_caption_with_pcm(vtt, 0).is_none());
    }

    #[test]
    fn managed_actual_short_film_raw_output_is_bounded_by_exact_decoded_samples() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/subtitle-recognition/base-film.raw.vtt"
        ));
        assert_eq!(
            hex::encode(Sha256::digest(raw.as_bytes())),
            "97e9bd9222522300ede5f533027a30e326510dcaafde41b9c2fb6d58d550e47d"
        );
        let result =
            intersect_caption_with_pcm(raw, 11_745_489).expect("real film output normalized");
        assert_eq!((result.clipped, result.discarded), (1, 0));
        assert!(result
            .vtt
            .contains("00:12:13.400 --> 00:12:14.093\n [BLANK_AUDIO]"));
        assert!(!result.vtt.contains("00:12:23.400"));
        assert!(complete_caption(&result.vtt));
    }

    #[test]
    fn decoded_pcm_guard_reads_samples_and_checks_cancellation_and_riff_bounds() {
        use std::io::{Seek, SeekFrom, Write};
        let root = crate::test_tempdir().expect("root");
        let path = root.path().join("audio.wav");
        write_pcm_fixture(&path, false);
        let cancel = CancellationToken::new();
        let open = || std::fs::File::open(&path).expect("PCM");
        assert_eq!(
            pcm_has_signal(&mut open(), &cancel),
            Ok((false, 16000)),
            "nonzero RIFF header is not speech"
        );
        write_pcm_fixture(&path, true);
        assert_eq!(
            pcm_has_signal(&mut open(), &cancel),
            Ok((true, 16000)),
            "one nonzero sample is never classified as digital silence"
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            pcm_has_signal(&mut open(), &cancelled),
            Err("worker_interrupted")
        );
        // A valid sparse 8s waveform permits cancellation after one bounded
        // sample read; the owned scanner returns before any next read.
        write_pcm_fixture(&path, false);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("writable");
        file.set_len(256044).expect("extend");
        file.seek(SeekFrom::Start(4)).expect("RIFF size");
        file.write_all(&256036_u32.to_le_bytes()).expect("size");
        file.seek(SeekFrom::Start(40)).expect("data size");
        file.write_all(&256000_u32.to_le_bytes()).expect("size");
        let during = CancellationToken::new();
        let mut reads = 0;
        assert_eq!(
            pcm_has_signal_observed(&mut file, &during, || {
                reads += 1;
                during.cancel();
            }),
            Err("worker_interrupted")
        );
        assert_eq!(reads, 1);
        for bytes in [b"not a waveform".to_vec(), {
            let mut bytes = std::fs::read(&path).expect("wave");
            bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
            bytes
        }] {
            std::fs::write(&path, bytes).expect("malformed");
            assert_eq!(pcm_has_signal(&mut open(), &cancel), Err("decode_failed"));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("oversize");
        file.set_len(MAX_WAV_BYTES + 1).expect("sparse bound");
        assert_eq!(pcm_has_signal(&mut open(), &cancel), Err("decode_failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_actual_decode_of_digital_silence_never_starts_recognizer() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("root");
        let (mut file, model, _, whisper) = process_fixture(root.path()).await;
        write_pcm_fixture(&file.path, false);
        let meta = std::fs::metadata(&file.path).expect("source");
        file.size = meta.len() as i64;
        file.mtime = meta
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let marker = root.path().join("recognizer-started");
        std::fs::write(
            &whisper,
            format!("#!/bin/sh\n/bin/touch '{}'\nexit 1\n", marker.display()),
        )
        .expect("recognizer");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700)).expect("mode");
        let handle = std::fs::File::open(&whisper).expect("held engine");
        let identity = plurx_core::fs_secure::std_file_identity(&handle).expect("engine identity");
        let source = std::fs::File::open(&file.path).expect("source");
        let source_identity =
            plurx_core::fs_secure::std_file_identity(&source).expect("source identity");
        let engine = crate::subtitle_models::artifacts::Engine {
            handle,
            identity,
            path: whisper.clone(),
            binary_sha256: hex::encode(Sha256::digest(
                std::fs::read(&whisper).expect("engine bytes"),
            )),
            adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
            platform: "test".into(),
            self_test_handle: source.try_clone().expect("source clone"),
            self_test_identity: source_identity,
            self_test_sha256: "test".into(),
        };
        let cancel = CancellationToken::new();
        // The test harness can run outside an installed package. Resolve its
        // real decoder before entering the managed LC_ALL-only child boundary;
        // production still refuses bare executable names.
        let decoder = executable(&crate::ffmpeg::ffmpeg_bin())
            .and_then(|path| path.canonicalize().ok())
            .expect("actual test FFmpeg executable");
        let error = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            pipeline(Pipeline {
                purpose: Purpose::Generation,
                runtime_cache_dir: root.path(),
                source: &source,
                source_path: &file.path,
                model: &model.handle,
                model_path: Path::new("unused-model-path"),
                command: &whisper,
                engine: Some(&engine),
                audio_index: 0,
                language: "en",
                cancel: &cancel,
                decoder: decoder.to_str().expect("decoder path"),
                managed: None,
            }),
        )
        .await
        .expect_err("managed digital silence refused");
        assert!(
            matches!(&error, StoreError::Task(code) if code == "no_speech_detected"),
            "actual decoder refusal: {error}"
        );
        assert!(
            !marker.exists(),
            "recognizer never receives wholly zero PCM"
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("subtitle-transcription"))
                .expect("scratch")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    async fn process_fixture(root: &Path) -> (MediaFile, Model, PathBuf, PathBuf) {
        use plurx_core::{
            domain::{AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult},
            store::{LibraryStore, MediaStore, SqliteStore},
        };
        use std::os::unix::fs::PermissionsExt;
        let store = SqliteStore::open_in_memory().expect("store");
        let library = store
            .create_library(&NewLibrary {
                name: "transcription fixture".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "synthetic".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let path = root.join("source.mkv");
        std::fs::write(&path, b"source").expect("source");
        let meta = std::fs::metadata(&path).expect("source meta");
        let mtime = meta
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let id = store
            .upsert_file(
                item,
                path.to_str().expect("path"),
                6,
                mtime,
                &ProbeResult {
                    raw_json: Some("{}".into()),
                    duration_ms: Some(1000),
                    video_codec: Some("h264".into()),
                    audio_streams: vec![AudioStream {
                        index: 0,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let file = store.get_file(id).await.expect("file").expect("file");
        let model_path = root.join("model.bin");
        std::fs::write(&model_path, b"model").expect("model");
        let model = open_model(model_path.to_str().expect("model path"))
            .await
            .expect("model fingerprint");
        let decoded = root.join("decoded.wav");
        write_pcm_fixture(&decoded, true);
        let decoder = root.join("decoder");
        std::fs::write(&decoder, format!("#!/bin/sh\nset -eu\ntest \"$(/bin/cat /dev/fd/3)\" = source\nfor last do :; done\n/bin/cp '{}' \"$last\"\n", decoded.display())).expect("decoder");
        let whisper = root.join("whisper");
        for executable in [&decoder, &whisper] {
            if executable.exists() {
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                    .expect("executable");
            }
        }
        (file, model, decoder, whisper)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_runaway_caption_is_stopped_at_the_child_file_limit() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("caption fixture");
        let (file, model, decoder, whisper) = process_fixture(root.path()).await;
        let size_probe = root.path().join("caption-size");
        std::fs::write(&whisper, format!(
            "#!/bin/sh\nfor last do :; done\nhead -c 8388608 /dev/zero > \"$last.vtt\"\nwc -c < \"$last.vtt\" > '{}'\nexit 0\n", size_probe.display()
        )).expect("runaway adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("adapter executable");
        let cancel = CancellationToken::new();
        let error = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            transcribe(
                root.path(),
                &file,
                &Settings::default(),
                &whisper,
                &model,
                &cancel,
                decoder.to_str().expect("decoder"),
            ),
        )
        .await
        .expect_err("runaway captions cannot publish");
        let written: u64 = std::fs::read_to_string(size_probe)
            .expect("child reported size")
            .trim()
            .parse()
            .expect("byte count");
        assert!(
            written <= (MAX_DOWNLOADED_SUBTITLE_BYTES + 1) as u64,
            "the OS stops growth before the child exits"
        );
        assert!(error.to_string().contains("exceeds bound"));
        assert_eq!(
            std::fs::read_dir(root.path().join("subtitle-transcription"))
                .expect("scratch")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_held_source_model_and_complete_caption_pipeline() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("canonical fixture");
        let (file, model, decoder, whisper) = process_fixture(root.path()).await;
        std::fs::write(&whisper,"#!/bin/sh\nset -eu\ntest \"$(cat /dev/fd/5)\" = model\nfor last do :; done\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:01.000\\nSynthetic speech.\\n' > \"$last.vtt\"\n").expect("adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let cancel = CancellationToken::new();
        let vtt = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            transcribe(
                root.path(),
                &file,
                &Settings::default(),
                &whisper,
                &model,
                &cancel,
                decoder.to_str().expect("decoder"),
            ),
        )
        .await
        .expect("complete bounded transcription");
        assert!(vtt.contains("Synthetic speech."));
        assert_eq!(
            std::fs::read_dir(root.path().join("subtitle-transcription"))
                .expect("scratch root")
                .count(),
            0,
            "scratch is removed before returning"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_pcm_preserves_nonzero_origin_delayed_audio_and_internal_gap() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("canonical fixture");
        let (mut file, model, _, whisper) = process_fixture(root.path()).await;
        let decoder = crate::ffmpeg::ffmpeg_bin();
        // Video defines container origin 7s. Audio starts at 7.4s, contains
        // speech at film .4-.6s, a timestamp gap, then speech at 1.0-1.4s.
        // Keep the discontinuity in packets rather than pre-inserting silence.
        let mut generate = tokio::process::Command::new(&decoder);
        generate
            .args([
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=2:d=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=16000:duration=0.6:samples_per_frame=160",
                "-filter:a",
                "asettb=1/16000,asetpts=PTS+6400+6400*gte(N\\,3200)",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "ffv1",
                "-c:a",
                "pcm_s16le",
                "-output_ts_offset",
                "7",
                "-t",
                "2",
            ])
            .arg(&file.path);
        let generated = plurx_core::process::bounded::output_command(
            &mut generate,
            Duration::from_secs(20),
            256 * 1024,
            plurx_core::process::ChildWork::background("transcription clock fixture"),
        )
        .await
        .expect("required FFmpeg must be available for PCM timeline regression");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let mut probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_packets",
                "-show_frames",
                "-show_entries",
                "format=start_time:stream=codec_type,start_time:packet=pts_time,duration_time:frame=pts_time,nb_samples",
                "-of",
                "json",
            ])
            .arg(&file.path);
        let probed = plurx_core::process::bounded::output_command(
            &mut probe,
            Duration::from_secs(20),
            256 * 1024,
            plurx_core::process::ChildWork::background("transcription clock fixture probe"),
        )
        .await
        .expect("required FFprobe must be available");
        assert!(probed.status.success());
        let facts: Value = serde_json::from_slice(&probed.stdout).expect("source clock facts");
        assert_eq!(facts["format"]["start_time"], "7.000000");
        let audio = facts["streams"]
            .as_array()
            .expect("streams")
            .iter()
            .find(|stream| stream["codec_type"] == "audio")
            .expect("selected audio");
        assert_eq!(
            audio["start_time"], "7.400000",
            "audio starts later than container origin"
        );
        // Prove the generated source itself contains a timestamp gap. Some
        // FFmpeg versions forward aselect audio without evaluating it; origin
        // timestamps alone would accept a continuous tone as this fixture.
        let clock = facts["packets_and_frames"]
            .as_array()
            .expect("source clock intervals");
        for kind in ["packet", "frame"] {
            let mut coverage: Vec<(f64, f64)> = Vec::new();
            let mut source_samples = 0_u64;
            for entry in clock.iter().filter(|entry| entry["type"] == kind) {
                let start = entry["pts_time"]
                    .as_str()
                    .expect("source PTS")
                    .parse::<f64>()
                    .expect("numeric source PTS");
                let duration = if kind == "frame" {
                    let count = entry["nb_samples"].as_u64().expect("decoded samples");
                    source_samples += count;
                    count as f64 / 16000.0
                } else {
                    entry["duration_time"]
                        .as_str()
                        .expect("packet duration")
                        .parse::<f64>()
                        .expect("numeric packet duration")
                };
                assert!(start.is_finite() && duration.is_finite() && duration > 0.0);
                let end = start + duration;
                if let Some(previous) = coverage.last_mut() {
                    assert!(
                        start + 0.000001 >= previous.1,
                        "no overlapping source intervals"
                    );
                    if (start - previous.1).abs() < 0.000001 {
                        previous.1 = end;
                        continue;
                    }
                }
                coverage.push((start, end));
            }
            assert_eq!(
                coverage.len(),
                2,
                "{kind}: exactly two source tone spans: {coverage:?}"
            );
            for (actual, expected) in coverage
                .iter()
                .zip([(7.4_f64, 7.6_f64), (8.0_f64, 8.4_f64)])
            {
                assert!(
                    (actual.0 - expected.0).abs() < 0.000001
                        && (actual.1 - expected.1).abs() < 0.000001,
                    "{kind}: exact delayed tone and internal source gap: {coverage:?}"
                );
            }
            if kind == "frame" {
                assert_eq!(
                    source_samples, 9600,
                    "source contains only the two tone spans"
                );
            }
        }
        let metadata = std::fs::metadata(&file.path).expect("generated source");
        file.size = metadata.len() as i64;
        file.mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        file.duration_ms = Some(2000);
        let captured = root.path().join("consumed.wav");
        std::fs::write(&whisper, format!(
            "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n case \"$1\" in\n --file) cp \"$2\" '{}'; shift;;\n --output-file) out=\"$2\"; shift;;\n esac\n shift\ndone\nprintf 'WEBVTT\\n\\n00:00:00.400 --> 00:00:01.400\\nSpeech on film time.\\n' > \"$out.vtt\"\n",
            captured.display()
        )).expect("adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let cancel = CancellationToken::new();
        let vtt = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            transcribe(
                root.path(),
                &file,
                &Settings::default(),
                &whisper,
                &model,
                &cancel,
                &decoder,
            ),
        )
        .await
        .expect("transcription");
        assert!(vtt.contains("00:00:00.400 --> 00:00:01.400"));
        let bytes = std::fs::read(captured).expect("PCM consumed by adapter");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let mut cursor = 12;
        let samples = loop {
            let header = bytes.get(cursor..cursor + 8).expect("WAV chunk header");
            let size = u32::from_le_bytes(header[4..8].try_into().expect("chunk size")) as usize;
            let payload = bytes.get(cursor + 8..cursor + 8 + size).expect("WAV chunk");
            if &header[..4] == b"data" {
                break payload
                    .chunks_exact(2)
                    .map(|sample| i16::from_le_bytes(sample.try_into().expect("PCM sample")))
                    .collect::<Vec<_>>();
            }
            cursor += 8 + size + (size & 1);
        };
        let peak = |start: usize, end: usize| {
            samples[start..end]
                .iter()
                .map(|sample| i32::from(*sample).abs())
                .max()
                .expect("sample interval")
        };
        assert_eq!(samples.len(), 22_400, "PCM spans film zero through 1.4s");
        assert_eq!(
            peak(0, 5600),
            0,
            "leading silence preserves selected-audio delay"
        );
        assert!(
            peak(7040, 8800) > 3000,
            "first speech retains film .44-.55s"
        );
        assert_eq!(peak(11200, 15200), 0, "internal packet gap remains silence");
        assert!(
            peak(17600, 20800) > 3000,
            "later speech remains at film 1.1-1.3s"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_zero_exit_decode_error_refuses_inference_and_publication() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("canonical fixture");
        let (file, model, decoder, whisper) = process_fixture(root.path()).await;
        std::fs::write(&decoder,
            "#!/bin/sh\nset -eu\nfor last do :; done\nhead -c 128 /dev/zero > \"$last\"\nprintf 'Error while decoding stream: corrupt audio packet\\n' >&2\nexit 0\n"
        ).expect("partial decoder");
        let invoked = root.path().join("inference-started");
        std::fs::write(
            &whisper,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", invoked.display()),
        )
        .expect("adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let cancel = CancellationToken::new();
        let error = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            transcribe(
                root.path(),
                &file,
                &Settings::default(),
                &whisper,
                &model,
                &cancel,
                decoder.to_str().expect("decoder"),
            ),
        )
        .await
        .expect_err("exit zero cannot authorize partial captions");
        assert!(error.to_string().contains("audio decode failed"));
        assert!(
            !invoked.exists(),
            "inference must never receive partial decoded audio"
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("subtitle-transcription"))
                .expect("scratch root")
                .count(),
            0,
            "scratch retires on refused decode"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_stop_reaps_owned_child_before_scratch_retirement() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("canonical fixture");
        let (file, model, decoder, whisper) = process_fixture(root.path()).await;
        std::fs::write(
            &whisper,
            "#!/bin/sh\nset -eu\nfor last do :; done\ntouch \"$last.started\"\nsleep 60\n",
        )
        .expect("adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let cancel = CancellationToken::new();
        let config = Settings::default();
        let operation = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            transcribe(
                root.path(),
                &file,
                &config,
                &whisper,
                &model,
                &cancel,
                decoder.to_str().expect("decoder"),
            ),
        );
        tokio::pin!(operation);
        let stop = async {
            loop {
                let started = std::fs::read_dir(root.path().join("subtitle-transcription"))
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .any(|entry| entry.path().join("caption.started").exists());
                if started {
                    cancel.cancel();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = &mut operation => panic!("adapter must remain running until Stop: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10),stop) => result.expect("child started"),
        }
        assert!(tokio::time::timeout(Duration::from_secs(5), operation)
            .await
            .expect("reap promptly")
            .is_err());
        assert_eq!(
            std::fs::read_dir(root.path().join("subtitle-transcription"))
                .expect("scratch root")
                .count(),
            0
        );
    }

    #[test]
    fn transcription_selects_matching_audio_ordinals_and_rejects_known_mismatch() {
        use plurx_core::domain::AudioStream;
        let track = |index, language: Option<&str>, default| AudioStream {
            index,
            language: language.map(str::to_owned),
            default,
            ..Default::default()
        };
        let dual = vec![
            track(0, Some("jpn"), true),
            track(5, Some("eng"), false),
            track(12, Some("en"), true),
        ];
        assert_eq!(selected_audio(&dual, "en"), Some(12));
        assert_eq!(selected_audio(&dual, "eng"), Some(12));
        assert_eq!(selected_audio(&dual, "ja"), Some(0));
        assert_eq!(selected_audio(&dual, "jpn"), Some(0));
        assert_eq!(
            selected_audio(&dual, "fr"),
            None,
            "known Japanese/English audio is not French"
        );
        assert_eq!(
            selected_audio(
                &[
                    track(7, Some("und"), true),
                    track(3, None, false),
                    track(1, Some("jpn"), true)
                ],
                "en"
            ),
            Some(7)
        );
        assert_eq!(
            selected_audio(
                &[track(7, Some("und"), true), track(3, Some("en-US"), false)],
                "eng"
            ),
            Some(3),
            "a real language match wins over default untagged audio"
        );
        assert!(language_matches("en", "eng"));
        assert!(language_matches("eng", "en"));
        assert_eq!(
            normalized_language("eng"),
            "en",
            "English-only model and CLI use canonical code"
        );
    }

    #[test]
    fn transcription_readiness_never_blocks_saved_enable() {
        Settings {
            enabled: true,
            model_path: "/missing/model.bin".into(),
            command: "/missing/whisper-cli".into(),
            ..Settings::default()
        }
        .validate()
        .expect("advisory readiness");
        assert!(!Settings::default().enabled);
        assert_eq!(Settings::default().interval_mins, 0);
        assert!(Settings {
            model_path: "relative/model.bin".into(),
            ..Settings::default()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn transcription_rejects_truncated_or_malformed_trailing_cues() {
        assert!(complete_caption(
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSpeech.\n"
        ));
        assert!(!complete_caption(
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSpeech.\n\n00:00:01.000 --> 00:00:02.000\n"
        ));
        assert!(!complete_caption(
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSpeech.\n\nbroken trailing cue\n"
        ));
    }

    #[test]
    fn transcription_existing_language_aliases_prevent_duplicate_sweep_work() {
        assert!(language_matches("eng", "en"));
        assert!(language_matches("GER", "de"));
        assert!(!language_matches("eng", "fr"));
    }
    #[test]
    fn managed_self_test_requires_recognizable_words_and_fixture_timing() {
        let good =
            "WEBVTT\n\n00:00:00.000 --> 00:00:11.000\nAsk what your country can do for you.\n";
        assert!(self_test_caption(good));
        assert!(!self_test_caption(&good.replace("country", "silence")));
        assert!(!self_test_caption(&good.replace("11.000", "11.001")));
        assert!(!self_test_caption(&good.replace("11.000", "59.000")));
        assert!(!self_test_caption("ask country you"));
    }

    #[tokio::test]
    async fn advanced_model_languages_uses_bounded_known_ggml_header() {
        let root = crate::test_tempdir().expect("root");
        let path = root.path().join("model.bin");
        for (vocabulary, expected) in [
            (51864_u32, 1),
            (51865, 99),
            (51866, 100),
            (51867, 0),
            (0, 0),
        ] {
            let mut bytes = 0x67676d6c_u32.to_le_bytes().to_vec();
            bytes.extend(vocabulary.to_le_bytes());
            std::fs::write(&path, bytes).expect("header");
            let languages = model_languages(path.to_str().expect("path")).await;
            assert_eq!(languages.len(), expected);
            assert_eq!(
                languages.iter().any(|code| code == "yue"),
                vocabulary == 51866
            );
        }
        std::fs::write(&path, b"not GGML").expect("unknown");
        assert!(model_languages(path.to_str().expect("path"))
            .await
            .is_empty());
        #[cfg(unix)]
        {
            let alias = root.path().join("model-link");
            std::os::unix::fs::symlink(&path, &alias).expect("alias");
            assert!(model_languages(alias.to_str().expect("path"))
                .await
                .is_empty());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcription_duration_admission_keeps_four_hour_ceiling() {
        let root = crate::test_tempdir().expect("root");
        let (mut file, _, _, _) = process_fixture(root.path()).await;
        for (duration, admitted) in [
            (None, false),
            (Some(0), false),
            (Some(-1), false),
            (Some(MAX_DURATION_MS), true),
            (Some(MAX_DURATION_MS + 1), false),
            (Some(i64::MAX), false),
        ] {
            file.duration_ms = duration;
            assert_eq!(eligible(&file), admitted, "duration {duration:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn advanced_known_51866_admits_and_executes_selected_language() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("root");
        let (file, _, decoder, whisper) = process_fixture(root.path()).await;
        let model_path = root.path().join("advanced.bin");
        let header =
            |vocabulary: u32| [0x67676d6c_u32.to_le_bytes(), vocabulary.to_le_bytes()].concat();
        std::fs::write(&model_path, header(51866)).expect("known header");
        let model = open_model(model_path.to_str().expect("path"))
            .await
            .expect("held Advanced model");
        assert_eq!(model.languages.len(), 100);
        assert!(model_supports_language(&model, "YUE"));
        assert!(model_supports_language(&model, "eng"));
        assert!(!model_supports_language(&model, "xyz"));
        let config = Settings {
            language: "yue".into(),
            ..Default::default()
        };
        let request = enqueue_request_selected(&file, &config, &model, &whisper, 0)
            .expect("accepted selected recipe");
        assert!(
            matches!(request.payload, JobPayload::SubtitleTranscribe { ref language, audio_index: 0, .. } if language == "yue")
        );
        std::fs::write(&whisper, "#!/bin/sh\nset -eu\nseen=0\nfor arg do test \"$arg\" != yue || seen=1; last=$arg; done\ntest $seen = 1\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:01.000\\nSynthetic selected speech.\\n' > \"$last.vtt\"\n").expect("adapter");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let cancel = CancellationToken::new();
        let vtt = transcribe_selected(
            root.path(),
            &file,
            &config,
            &whisper,
            &model,
            &cancel,
            decoder.to_str().expect("decoder"),
            0,
        )
        .await
        .expect("actual selected child");
        assert!(vtt.contains("Synthetic selected speech."));
        for (vocabulary, en, yue) in [
            (51864, true, false),
            (51865, true, false),
            (51867, false, false),
        ] {
            std::fs::write(&model_path, header(vocabulary)).expect("comparison header");
            let other = open_model(model_path.to_str().expect("path"))
                .await
                .expect("comparison model");
            assert_eq!(model_supports_language(&other, "en"), en);
            assert_eq!(model_supports_language(&other, "yue"), yue);
        }
        let oversized = std::fs::OpenOptions::new()
            .write(true)
            .open(&model_path)
            .expect("size bound fixture");
        oversized
            .set_len(MAX_MODEL_BYTES + 1)
            .expect("sparse bound");
        assert!(model_languages(model_path.to_str().expect("path"))
            .await
            .is_empty());
        assert!(open_model(model_path.to_str().expect("path"))
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_pipeline_executes_held_engine_and_exact_audio_language() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_tempdir().expect("root");
        let (file, model, decoder, whisper) = process_fixture(root.path()).await;
        std::fs::write(&decoder, format!("#!/bin/sh\nset -eu\ntest \"$(/bin/cat /dev/fd/3)\" = source\nseen=0\nfor arg do test \"$arg\" != 0:a:7 || seen=1; last=$arg; done\ntest $seen = 1\n/bin/cp '{}' \"$last\"\n", root.path().join("decoded.wav").display())).expect("decoder");
        std::fs::write(&whisper, "#!/bin/sh\nset -eu\ncase \"$0\" in /dev/fd/7|*/whisper-cli) ;; *) exit 1 ;; esac\ntest \"$(/bin/cat /dev/fd/5)\" = model\nseen=0\nfor arg do test \"$arg\" != fr || seen=1; last=$arg; done\ntest $seen = 1\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:01.000\\nSynthetic speech.\\n' > \"$last.vtt\"\n").expect("engine");
        for path in [&decoder, &whisper] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
        }
        let handle = std::fs::File::open(&whisper).expect("held engine");
        let identity = plurx_core::fs_secure::std_file_identity(&handle).expect("identity");
        let fixture = std::fs::File::open(&file.path).expect("held source");
        let fixture_identity =
            plurx_core::fs_secure::std_file_identity(&fixture).expect("fixture identity");
        let engine = crate::subtitle_models::artifacts::Engine {
            handle,
            identity,
            path: root.path().join("missing-engine-path"),
            binary_sha256: hex::encode(Sha256::digest(
                std::fs::read(&whisper).expect("engine bytes"),
            )),
            adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
            platform: "test".into(),
            self_test_handle: fixture.try_clone().expect("fixture clone"),
            self_test_identity: fixture_identity,
            self_test_sha256: "test".into(),
        };
        let cancel = CancellationToken::new();
        let input = Pipeline {
            purpose: Purpose::Generation,
            runtime_cache_dir: root.path(),
            source: &fixture,
            source_path: &file.path,
            model: &model.handle,
            model_path: Path::new("missing-model-path"),
            command: &engine.path,
            engine: Some(&engine),
            audio_index: 7,
            language: "fr",
            cancel: &cancel,
            decoder: decoder.to_str().expect("decoder"),
            managed: None,
        };
        assert!(
            matches!(input.child_failure(std::io::Error::from(std::io::ErrorKind::TimedOut), true), StoreError::Task(code) if code == "inference_timeout")
        );
        assert!(
            matches!(input.child_failure(std::io::Error::from(std::io::ErrorKind::Other), true), StoreError::Task(code) if code == "inference_failed")
        );
        assert!(
            matches!(input.child_failure(std::io::Error::from(std::io::ErrorKind::TimedOut), false), StoreError::Task(code) if code == "decode_failed")
        );
        assert!(
            matches!(input.child_failure(std::io::Error::from(std::io::ErrorKind::Interrupted), true), StoreError::Task(code) if code == "worker_interrupted")
        );
        let output = pipeline(input).await.expect("held exact input pipeline");
        assert!(output.contains("Synthetic speech."));
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt;
            let stage = private_attempt(root.path()).expect("private stage");
            let invocation = engine_invocation(&engine, stage.path(), &cancel)
                .await
                .expect("qualified copy");
            assert!(invocation.unchanged());
            assert_eq!(
                std::fs::metadata(&invocation.program)
                    .expect("stage metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o500
            );
            assert_eq!(
                hex::encode(Sha256::digest(
                    std::fs::read(&invocation.program).expect("stage bytes")
                )),
                engine.binary_sha256
            );
            assert!(
                matches!(engine_invocation(&engine, stage.path(), &cancel).await, Err(StoreError::Task(code)) if code == "engine_staging_failed"),
                "exclusive staging never replaces an existing target"
            );
            let altered_stage = private_attempt(root.path()).expect("bad digest stage");
            let mut wrong_engine = engine;
            wrong_engine.binary_sha256 = "0".repeat(64);
            assert!(
                matches!(engine_invocation(&wrong_engine, altered_stage.path(), &cancel).await, Err(StoreError::Task(code)) if code == "packaged_engine_changed")
            );
        }
    }
    /// Explicit local qualification: uses the installed package and immutable
    /// base.en model, while the actual children receive only LC_ALL=C.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires PLURX_SUBTITLE_ENGINE, PLURX_TEST_SUBTITLE_MODEL and packaged ffmpeg"]
    async fn managed_real_engine_held_fd_clean_environment_recognizes_fixture() {
        let engine = crate::subtitle_models::artifacts::inspect_engine()
            .await
            .expect("qualified engine package");
        let catalog: Value =
            serde_json::from_str(include_str!("subtitle_models/catalog.json")).expect("catalog");
        let spec: crate::subtitle_models::artifacts::CatalogModel = serde_json::from_value(
            catalog["models"]
                .as_array()
                .expect("models")
                .iter()
                .find(|model| model["id"] == "base.en")
                .expect("base.en")
                .clone(),
        )
        .expect("model spec");
        let root = crate::test_tempdir().expect("root");
        let models = root.path().join("models");
        std::fs::create_dir(&models).expect("models");
        std::fs::copy(
            std::env::var_os("PLURX_TEST_SUBTITLE_MODEL").expect("base.en bytes"),
            models.join(format!("{}.bin", spec.sha256)),
        )
        .expect("stage qualified model");
        let model = crate::subtitle_models::artifacts::open_model(&models, &spec)
            .await
            .expect("verified model");
        let decoder = crate::ffmpeg::ffmpeg_bin();
        let cancel = CancellationToken::new();
        let vtt = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            pipeline(Pipeline {
                purpose: Purpose::SelfTest,
                runtime_cache_dir: root.path(),
                source: &engine.self_test_handle,
                source_path: Path::new(""),
                model: &model.handle,
                model_path: &model.path,
                command: &engine.path,
                engine: Some(&engine),
                audio_index: 0,
                language: "en",
                cancel: &cancel,
                decoder: &decoder,
                managed: None,
            }),
        )
        .await
        .expect("actual recognizer through production pipeline");
        assert!(
            self_test_caption(&vtt),
            "known speech and bounded timed cues: {vtt}"
        );
        managed_artifacts_unchanged(&engine, &model).expect("held artifacts unchanged");
        assert_eq!(
            plurx_core::fs_secure::std_file_identity(&engine.self_test_handle).expect("fixture"),
            engine.self_test_identity
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn advanced_explicit_audio_and_language_are_frozen_in_request() {
        use plurx_core::domain::AudioStream;
        let root = crate::test_tempdir().expect("root");
        let (mut file, model, _, whisper) = process_fixture(root.path()).await;
        file.audio_streams = vec![
            AudioStream {
                index: 0,
                language: Some("eng".into()),
                default: false,
                ..Default::default()
            },
            AudioStream {
                index: 7,
                language: Some("en".into()),
                default: true,
                ..Default::default()
            },
            AudioStream {
                index: 12,
                language: Some("  UND  ".into()),
                ..Default::default()
            },
        ];
        assert_eq!(selected_audio(&file.audio_streams, "en"), Some(7));
        assert!(accepted_audio(&file, 0, "en"));
        assert!(!accepted_audio(&file, 0, "fr"));
        assert!(accepted_audio(&file, 12, "fr"));
        assert!(!accepted_audio(&file, 99, "en"));
        let config = Settings {
            language: "en".into(),
            ..Default::default()
        };
        let request =
            enqueue_request_selected(&file, &config, &model, &whisper, 0).expect("request");
        let JobPayload::SubtitleTranscribe {
            audio_index,
            language,
            pipeline_digest: digest,
            ..
        } = request.payload
        else {
            panic!("legacy exact request");
        };
        assert_eq!(audio_index, 0);
        assert_eq!(language, "en");
        assert_eq!(digest, pipeline_digest(&config, &whisper, &model, 0));
        assert_ne!(digest, pipeline_digest(&config, &whisper, &model, 7));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn accepted_advanced_work_survives_disable_but_foreground_yield_reaps() {
        use plurx_core::{
            store::{SqliteStore, Store},
            transcode::{EncoderCaps, Pipeline as VideoPipeline},
        };
        use std::os::unix::fs::PermissionsExt;
        for foreground_yield in [false, true] {
            let root = crate::test_tempdir().expect("root");
            let (file, model, decoder, whisper) = process_fixture(root.path()).await;
            let release = root.path().join("release");
            std::fs::write(&whisper, format!("#!/bin/sh\nset -eu\nfor last do :; done\ntouch \"$last.started\"\nwhile test ! -e '{}'; do /bin/sleep 0.05; done\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:01.000\\nSynthetic speech.\\n' > \"$last.vtt\"\n", release.display())).expect("adapter");
            std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let manager = crate::transcode::TranscodeManager::new(
                Arc::clone(&store),
                root.path().join("work"),
                EncoderCaps::default(),
                VideoPipeline::Cpu,
            );
            let admission = manager.admit_fragment().await.expect("idle capacity");
            let cancel = CancellationToken::new();
            let shutdown = CancellationToken::new();
            let config = Settings::default();
            let inference = plurx_core::process::bounded::cancellable(
                cancel.clone(),
                transcribe(
                    root.path(),
                    &file,
                    &config,
                    &whisper,
                    &model,
                    &cancel,
                    decoder.to_str().expect("decoder"),
                ),
            );
            let operation =
                await_claimed_inference(inference, &manager, &admission, &shutdown, &cancel);
            tokio::pin!(operation);
            let started = async {
                loop {
                    if std::fs::read_dir(root.path().join("subtitle-transcription"))
                        .ok()
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .any(|entry| entry.path().join("caption.started").exists())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            tokio::select! {
                result = &mut operation => panic!("attempt must still run: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(10), started) => result.expect("child starts"),
            }
            store
                .put_settings(&[
                    ("subtitles.transcription.config", r#"{"enabled":false}"#),
                    (
                        "subtitles.transcription.managed.v1",
                        r#"{"mode":"managed"}"#,
                    ),
                ])
                .await
                .expect("disable and change mode");
            assert!(
                tokio::time::timeout(Duration::from_millis(300), &mut operation)
                    .await
                    .is_err(),
                "saved disable/mode change cannot cancel accepted work"
            );
            let demand = foreground_yield.then(|| manager.test_mark_live_waiting());
            if !foreground_yield {
                std::fs::write(&release, b"done").expect("finish inference");
            }
            let result = tokio::time::timeout(Duration::from_secs(5), operation)
                .await
                .expect("finish or reap promptly");
            assert_eq!(result.is_err(), foreground_yield);
            assert_eq!(cancel.is_cancelled(), foreground_yield);
            assert_eq!(
                std::fs::read_dir(root.path().join("subtitle-transcription"))
                    .expect("scratch")
                    .count(),
                0,
                "children reaped before scratch retirement"
            );
            drop(demand);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn managed_source_unavailable_is_retryable_before_any_child() {
        let root = crate::test_tempdir().expect("root");
        let (file, _, _, _) = process_fixture(root.path()).await;
        let source = managed_source(&file, None).await.expect("source present");
        std::fs::remove_file(&file.path).expect("worker loses local media access");
        assert!(
            matches!(managed_source(&file, None).await, Err(StoreError::Task(code)) if code == "worker_source_unavailable")
        );
        assert!(
            matches!(managed_source(&file, Some(source.object_version())).await, Err(StoreError::Task(code)) if code == "worker_source_unavailable")
        );
        assert!(
            !root.path().join("subtitle-transcription").exists(),
            "source refusal precedes decoder, recognizer and executable staging"
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn managed_shutdown_reaps_child_before_private_scratch_and_admission_retire() {
        use plurx_core::store::{
            background_jobs::{ClaimJob, EnqueueOutcome, JobKind},
            background_jobs_transcription::ModelAction,
            SqliteStore, Store,
        };
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "PLURX_TEST_MANAGED_SHUTDOWN_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let root = crate::test_tempdir().expect("decoder root");
            let decoder = root.path().join("decoder");
            let decoded = root.path().join("decoded.wav");
            write_pcm_fixture(&decoded, true);
            std::fs::write(
                &decoder,
                format!(
                    "#!/bin/sh\nset -eu\nfor last do :; done\n/bin/cp '{}' \"$last\"\n",
                    decoded.display()
                ),
            )
            .expect("decoder");
            std::fs::set_permissions(&decoder, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            let output = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "subtitle_transcription::tests::managed_shutdown_reaps_child_before_private_scratch_and_admission_retire", "--nocapture"])
                .env(CHILD, "1").env("PLURX_FFMPEG", &decoder).kill_on_drop(true).output())
                .await.expect("isolated shutdown test bounded").expect("child test");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = crate::test_tempdir().expect("private root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let state = AppState::new(
            "Managed shutdown test".into(),
            Arc::clone(&store),
            crate::state::Dirs {
                artwork: root.path().join("artwork"),
                transcode: root.path().join("transcode"),
                cache: root.path().join("cache"),
                subs: root.path().join("subs"),
                runtime_cache: root.path().join("runtime"),
                renditions: root.path().join("renditions"),
            },
            "shutdown-worker".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        let whisper = root.path().join("recognizer");
        let script = b"#!/bin/sh\nset -eu\nfor last do :; done\nprintf '%s' $$ > \"$last.pid\"\ntouch \"$last.started\"\n/bin/sleep 60\n";
        std::fs::write(&whisper, script).expect("recognizer");
        std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o700))
            .expect("recognizer mode");
        let handle = std::fs::File::open(&whisper).expect("held recognizer");
        let identity =
            plurx_core::fs_secure::std_file_identity(&handle).expect("recognizer identity");
        let fixture = root.path().join("self-test.wav");
        let fixture_bytes = include_bytes!("subtitle_models/self-test.wav");
        std::fs::write(&fixture, fixture_bytes).expect("speech fixture");
        let fixture = std::fs::File::open(fixture).expect("held fixture");
        let fixture_identity =
            plurx_core::fs_secure::std_file_identity(&fixture).expect("fixture identity");
        let engine = crate::subtitle_models::artifacts::Engine {
            handle,
            path: whisper,
            binary_sha256: hex::encode(Sha256::digest(script)),
            adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
            platform: "synthetic-lifecycle-test".into(),
            identity,
            self_test_handle: fixture,
            self_test_identity: fixture_identity,
            self_test_sha256: hex::encode(Sha256::digest(fixture_bytes)),
        };
        let model_path = root.path().join("model.bin");
        std::fs::write(&model_path, b"model").expect("model");
        let model_handle = std::fs::File::open(&model_path).expect("held model");
        let model = crate::subtitle_models::artifacts::ManagedModel {
            identity: plurx_core::fs_secure::std_file_identity(&model_handle)
                .expect("model identity"),
            handle: model_handle,
            path: model_path,
            sha256: hex::encode(Sha256::digest(b"model")),
        };
        let payload = JobPayload::SubtitleModelOperation {
            translation_profile: None,
            action: ModelAction::Verify,
            model_id: "base.en".into(),
            catalog_revision: crate::subtitle_models::catalog().revision.clone(),
            model_sha256: model.sha256.clone(),
            adapter_contract: engine.adapter_contract.clone(),
            target_node_id: state.node_id.clone(),
        };
        let now = crate::state::clock_ms();
        let job_id = uuid::Uuid::new_v4().to_string();
        let outcome = store
            .enqueue_job(EnqueueJob {
                id: job_id.clone(),
                payload,
                dedupe_key: format!("shutdown:{job_id}"),
                priority: 0,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "subtitle-model".into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    request_digest: "d".repeat(64),
                    consumer_kind: "subtitle_model_operation".into(),
                    consumer_ref: "d".repeat(64),
                    target_node_id: Some(state.node_id.clone()),
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue owned model test");
        assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
        let candidate = store
            .job_candidates(CandidateQuery {
                node_id: state.node_id.clone(),
                kinds: vec![JobKind::SubtitleModelOperation],
                after: None,
                now_ms: now,
                limit: 1,
            })
            .await
            .expect("candidate")
            .jobs
            .into_iter()
            .next()
            .expect("owned candidate");
        let authority = state.jobs.execution_authority();
        let (claimed, deadline) = crate::background_jobs::claim_with_resolution(
            store.as_ref(),
            &candidate,
            ClaimJob {
                job_id,
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: state.subtitle_models.boot_id.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleModelOperation,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            },
        )
        .await
        .expect("claim")
        .expect("ownership granted");
        let active = crate::background_jobs::ActiveBackgroundJob::start(
            Arc::clone(&store),
            authority,
            claimed.token.expect("token"),
            deadline,
            JobKind::SubtitleModelOperation,
        )
        .expect("active claim");
        let fence = active.fence();
        let manager_cancel = fence.loss_token().child_token();
        store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
            .await
            .expect("two-thread speech budget");
        let admission = state
            .transcode
            .admit_speech()
            .await
            .expect("idle speech admission");
        assert!(state.transcode.speech_capacity_available(&admission));
        let mut operation = Box::pin(self_test_managed(
            &state,
            &engine,
            &model,
            &fence,
            &admission,
            &manager_cancel,
        ));
        let scratch = scratch_cache(&state).join("subtitle-transcription");
        let started = async {
            loop {
                if let Some(path) = std::fs::read_dir(&scratch)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path().join("caption.started"))
                    .find(|path| path.exists())
                {
                    let pid = std::fs::read_to_string(path.with_file_name("caption.pid"))
                        .expect("child PID")
                        .parse::<i32>()
                        .expect("PID");
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let pid = tokio::select! {
            result = &mut operation => panic!("recognizer must be running before shutdown: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), started) => result.expect("recognizer starts"),
        };
        assert!(
            state.transcode.admit_fragment().await.is_none(),
            "admission remains owned while child runs"
        );
        manager_cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(5), &mut operation)
            .await
            .expect("shutdown drains promptly")
            .is_err());
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "recognizer no longer exists before returning"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            std::fs::read_dir(&scratch).expect("scratch root").count(),
            0,
            "private attempt retires after child reaping"
        );
        assert!(
            state.transcode.admit_fragment().await.is_none(),
            "caller retains physical admission through child cleanup"
        );
        drop(operation);
        drop(admission);
        assert!(
            state.transcode.admit_fragment().await.is_some(),
            "physical admission reusable after cleanup"
        );
        let _ = fence
            .settle(JobSettlement::Yield {
                error_code: Some("worker_interrupted".into()),
                checkpoint: None,
                not_before_ms: now + 5000,
            })
            .await
            .expect("settle owned attempt");
        active.finish().await;
    }
}
