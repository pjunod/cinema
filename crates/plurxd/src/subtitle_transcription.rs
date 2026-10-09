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
            CandidateQuery, ClaimJob, EnqueueJob, JobKind, JobPayload, JobRequest, JobSettlement,
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
    fn validate(&self) -> Result<(), ApiError> {
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
    let raw =
        serde_json::to_string(&config).map_err(|error| ApiError::Internal(error.to_string()))?;
    state
        .store
        .put_settings(&[
            (&format!("{PREFIX}config"), &raw),
            (&format!("{PREFIX}next_run"), "0"),
        ])
        .await?;
    settings(admin, State(state)).await
}

struct Model {
    handle: std::fs::File,
    digest: String,
    identity: plurx_core::fs_secure::FileIdentity,
    english_only: bool,
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
            english_only = u32::from_le_bytes(buffer[..4].try_into().expect("header width"))
                == 0x67676d6c
                && u32::from_le_bytes(buffer[4..8].try_into().expect("header width")) == 51864;
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
fn eligible(file: &MediaFile) -> bool {
    file.probed
        && file.size > 0
        && !file.audio_streams.is_empty()
        && file
            .duration_ms
            .is_some_and(|duration| duration > 0 && duration <= MAX_DURATION_MS)
}
fn normalized_language(value: &str) -> String {
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
    let audio_map = format!("0:a:{audio_index}");
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
        .await
        .map_err(StoreError::Task)?;
    let scratch_root = runtime_cache_dir.join("subtitle-transcription");
    tokio::fs::create_dir_all(&scratch_root)
        .await
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let scratch = tempfile::Builder::new()
        .prefix("attempt-")
        .tempdir_in(&scratch_root)
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let wav = scratch.path().join("audio.wav");
    let mut decode = tokio::process::Command::new(decoder);
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
        "2",
        "-i",
    ]);
    #[cfg(unix)]
    {
        decode.arg("/dev/fd/3");
        crate::ffmpeg::inherit_file_descriptors(&mut decode, &[(&source.handle, 3)]);
    }
    #[cfg(windows)]
    {
        crate::ffmpeg::verify_windows_source_path(&source.handle, &file.path)
            .map_err(StoreError::Task)?;
        decode.arg(&file.path);
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
            "2",
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
    let output = plurx_core::process::bounded::output_command(
        &mut decode,
        Duration::from_secs(30 * 60),
        256 * 1024,
        plurx_core::process::ChildWork::background("subtitle transcription audio"),
    )
    .await
    .map_err(|_| StoreError::Task("transcription audio decode failed or interrupted".into()))?;
    // Under -v error any diagnostic is an error, even if a demuxer/decoder
    // tolerates partial corruption and exits zero. Never let that partial
    // waveform become complete captions that suppress later discovery.
    if !output.status.success() || output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(StoreError::Task("transcription audio decode failed".into()));
    }
    let meta = tokio::fs::symlink_metadata(&wav)
        .await
        .map_err(|error| StoreError::Task(error.to_string()))?;
    if !meta.is_file() || meta.len() <= 44 || meta.len() > MAX_WAV_BYTES {
        return Err(StoreError::Task(
            "transcription audio exceeds four-hour bound".into(),
        ));
    }
    // The model fingerprint is held across attempts; reset the shared open
    // description before passing its descriptor to a new child.
    use std::io::{Seek, SeekFrom};
    (&model.handle)
        .seek(SeekFrom::Start(0))
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let output_base = scratch.path().join("caption");
    let mut whisper = tokio::process::Command::new(command);
    let whisper_language = normalized_language(&config.language);
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
        crate::ffmpeg::inherit_file_descriptors(&mut whisper, &[(&model.handle, 5)]);
    }
    #[cfg(windows)]
    {
        crate::ffmpeg::verify_windows_source_path(&model.handle, Path::new(&config.model_path))
            .map_err(StoreError::Task)?;
        whisper.arg(&config.model_path);
    }
    whisper
        .arg("--file")
        .arg(&wav)
        .arg("--output-file")
        .arg(&output_base);
    let output = plurx_core::process::bounded::output_command(
        &mut whisper,
        WALL,
        256 * 1024,
        plurx_core::process::ChildWork::background("subtitle transcription whisper.cpp"),
    )
    .await
    .map_err(|_| StoreError::Task("transcription command failed or interrupted".into()))?;
    if !output.status.success() || cancel.is_cancelled() {
        return Err(StoreError::Task(
            "transcription command failed or interrupted".into(),
        ));
    }
    if !source.unchanged()
        || plurx_core::fs_secure::std_file_identity(&model.handle)
            .ok()
            .as_ref()
            != Some(&model.identity)
    {
        return Err(StoreError::Task(
            "transcription source or model changed".into(),
        ));
    }
    // A replacement at the source pathname also invalidates publication.
    crate::fragment_index_cluster::open_source_playback_fence(file, Some(source.object_version()))
        .await
        .map_err(StoreError::Task)?;
    let path = output_base.with_extension("vtt");
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| StoreError::Task("transcription produced no captions".into()))?;
    if !meta.is_file() || meta.len() > MAX_DOWNLOADED_SUBTITLE_BYTES as u64 {
        return Err(StoreError::Task(
            "transcription caption output exceeds bound".into(),
        ));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take((MAX_DOWNLOADED_SUBTITLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let vtt = String::from_utf8(bytes)
        .map_err(|_| StoreError::Task("transcription captions are not UTF-8".into()))?
        .replace("\r\n", "\n");
    if !complete_caption(&vtt) {
        return Err(StoreError::Task(
            "transcription did not produce complete valid captions".into(),
        ));
    }
    Ok(vtt)
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
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if let Err(error) = pass(&state, &boot, &shutdown, &mut model_cache).await {
            tracing::debug!(%error,"offline subtitle transcription pass stopped");
        }
        tokio::select! { ()=shutdown.cancelled()=>return, ()=tokio::time::sleep(Duration::from_secs(15))=>{} }
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
    if model.english_only && normalized_language(&config.language) != "en" {
        return Ok(());
    }
    if !state.transcode.fragment_worker_idle(&admission) {
        return Ok(());
    }
    if config.interval_mins > 0 {
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
            ..
        } = payload
        else {
            unreachable!()
        };
        if pipeline_digest != &self::pipeline_digest(&config, &command, model, audio_index) {
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
                || selected_audio(&file.audio_streams, &config.language) != Some(audio_index)
            {
                return Err(StoreError::Task(
                    "transcription source changed or exceeds bound".into(),
                ));
            }
            plurx_core::process::bounded::cancellable(
                cancel.clone(),
                transcribe(
                    &scratch_cache(state),
                    &file,
                    &config,
                    &command,
                    model,
                    &cancel,
                    &crate::ffmpeg::ffmpeg_bin(),
                ),
            )
            .await
        };
        tokio::pin!(operation);
        let result = tokio::select! {
            result=&mut operation=>result,
            ()=shutdown.cancelled()=>{ cancel.cancel(); operation.await },
            ()=watch_enabled(state,&admission)=>{ cancel.cancel(); operation.await },
        };
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
                        source_size,
                        source_mtime,
                        provider_file_id: 0,
                        transcription: Some(SubtitleTranscription {
                            audio_index,
                            artifact_key: artifact_key(&payload)?,
                            model_sha256: model.digest.clone(),
                            pipeline_digest: self::pipeline_digest(
                                &config,
                                &command,
                                model,
                                audio_index,
                            ),
                            adapter: "whisper.cpp".into(),
                            generated_at_ms: crate::state::clock_ms(),
                        }),
                        language: config.language.clone(),
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
async fn watch_enabled(state: &AppState, admission: &crate::transcode::FragmentAdmission) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if !state.transcode.fragment_worker_idle(admission)
            || !read_settings(state)
                .await
                .is_ok_and(|settings| settings.enabled)
        {
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
        let decoder = root.join("decoder");
        std::fs::write(&decoder,"#!/bin/sh\nset -eu\ntest \"$(cat /dev/fd/3)\" = source\nfor last do :; done\nhead -c 128 /dev/zero > \"$last\"\n").expect("decoder");
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
                "sine=frequency=1000:sample_rate=16000:duration=1",
                "-filter:a",
                "aselect=lt(t\\,0.2)+gte(t\\,0.6),asetpts=PTS+0.4/TB",
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
                "-show_entries",
                "format=start_time:stream=codec_type,start_time",
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
}
