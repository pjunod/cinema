//! Permanent Dolby Vision Profile 7 to 8.1 conversion.
//!
//! This is intentionally separate from the playback copy pipe. The output is
//! proved as a complete replacement before the source pathname moves, and the
//! scanner then sees an ordinary Profile 8 file on its next read.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use plurx_core::domain::{MediaFile, ProbeResult};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_OUTPUT: usize = 32 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ToolCapability {
    pub command: String,
    pub version: Option<String>,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvDiskCapabilities {
    pub available: bool,
    pub dovi_tool: ToolCapability,
    pub mkvmerge: ToolCapability,
    pub reason: Option<String>,
}

impl DvDiskCapabilities {
    pub fn unavailable_reason(&self) -> Option<&str> {
        (!self.available)
            .then_some(self.reason.as_deref())
            .flatten()
    }
}

#[derive(Clone, Debug)]
pub struct DvDiskTools {
    ffmpeg: String,
    dovi_tool: String,
    mkvmerge: String,
}

impl DvDiskTools {
    pub fn from_environment() -> Self {
        Self {
            ffmpeg: crate::ffmpeg::ffmpeg_bin(),
            dovi_tool: tool_from_env("PLURX_DOVI_TOOL", "dovi_tool"),
            mkvmerge: tool_from_env("PLURX_MKVMERGE", "mkvmerge"),
        }
    }

    #[cfg(test)]
    fn new(ffmpeg: String, dovi_tool: String, mkvmerge: String) -> Self {
        Self {
            ffmpeg,
            dovi_tool,
            mkvmerge,
        }
    }
}

fn tool_from_env(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

pub async fn probe_capabilities() -> DvDiskCapabilities {
    let tools = DvDiskTools::from_environment();
    probe_capabilities_with(&tools).await
}

async fn probe_capabilities_with(tools: &DvDiskTools) -> DvDiskCapabilities {
    let dovi_tool = probe_tool(&tools.dovi_tool).await;
    let mut mkvmerge = probe_tool(&tools.mkvmerge).await;
    if mkvmerge.available
        && mkvmerge
            .version
            .as_deref()
            .and_then(mkvmerge_major)
            .is_none_or(|major| major < 68)
    {
        mkvmerge.available = false;
        mkvmerge.reason = Some("mkvmerge 68 or newer is required".to_owned());
    }
    let reason = if !dovi_tool.available {
        Some(format!(
            "dovi_tool unavailable: {}",
            dovi_tool
                .reason
                .as_deref()
                .unwrap_or("version probe failed")
        ))
    } else if !mkvmerge.available {
        Some(format!(
            "mkvmerge unavailable: {}",
            mkvmerge.reason.as_deref().unwrap_or("version probe failed")
        ))
    } else {
        None
    };
    DvDiskCapabilities {
        available: reason.is_none(),
        dovi_tool,
        mkvmerge,
        reason,
    }
}

async fn probe_tool(command: &str) -> ToolCapability {
    let output = tokio::process::Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => {
            let text = if output.stdout.is_empty() {
                &output.stderr
            } else {
                &output.stdout
            };
            let version = String::from_utf8_lossy(text)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned);
            ToolCapability {
                command: command.to_owned(),
                available: version.is_some(),
                reason: version
                    .is_none()
                    .then(|| format!("{command} --version returned no version")),
                version,
            }
        }
        Ok(output) => ToolCapability {
            command: command.to_owned(),
            version: None,
            available: false,
            reason: Some(format!(
                "{command} --version exited with {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
            )),
        },
        Err(error) => ToolCapability {
            command: command.to_owned(),
            version: None,
            available: false,
            reason: Some(format!("{command} not found: {error}")),
        },
    }
}

fn mkvmerge_major(version: &str) -> Option<u64> {
    let marker = version.find("mkvmerge v")? + "mkvmerge v".len();
    version[marker..].split('.').next()?.parse().ok()
}

#[derive(Clone, Debug)]
pub struct VerifiedReplacement {
    pub el_type: Option<&'static str>,
    pub bytes_after: i64,
}

#[derive(Clone, Debug)]
pub struct PublishedReplacement {
    pub original_path: Option<String>,
    pub bytes_after: i64,
    pub probe: ProbeResult,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Clone, Debug)]
pub struct ConversionPaths {
    pub directory: PathBuf,
    pub raw: PathBuf,
    pub converted: PathBuf,
    pub rpu: PathBuf,
    pub replacement: PathBuf,
    pub staged_original: PathBuf,
    pub retained_original: PathBuf,
}

impl ConversionPaths {
    pub fn for_file(file: &MediaFile) -> Result<Self, String> {
        let parent = file
            .path
            .parent()
            .ok_or_else(|| "source has no parent directory".to_owned())?;
        let name = file
            .path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| "source filename is not valid UTF-8".to_owned())?;
        let directory = parent.join(format!(".{name}.plurx-dv-{}", file.id));
        Ok(Self {
            raw: directory.join("BL_RPU.hevc"),
            converted: directory.join("BL_RPU.p81.hevc"),
            rpu: directory.join("RPU.bin"),
            replacement: directory.join("replacement.mkv"),
            staged_original: directory.join("source.p7.original"),
            retained_original: PathBuf::from(format!("{}.p7.orig", file.path.display())),
            directory,
        })
    }
}

pub async fn build_and_verify(
    tools: &DvDiskTools,
    file: &MediaFile,
    loss: &CancellationToken,
) -> Result<VerifiedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    prepare_fresh_directory(&paths, file).await?;

    run_tool(
        &tools.ffmpeg,
        &[
            "-nostdin".into(),
            "-v".into(),
            "error".into(),
            "-i".into(),
            file.path.as_os_str().to_owned(),
            "-map".into(),
            "0:v:0".into(),
            "-c:v".into(),
            "copy".into(),
            "-bsf:v".into(),
            "hevc_mp4toannexb,filter_units=remove_types=63".into(),
            "-f".into(),
            "hevc".into(),
            paths.raw.as_os_str().to_owned(),
        ],
        loss,
    )
    .await?;
    require_nonempty(&paths.raw).await?;

    run_tool(
        &tools.dovi_tool,
        &[
            "-m".into(),
            "2".into(),
            "convert".into(),
            "--discard".into(),
            "-i".into(),
            paths.raw.as_os_str().to_owned(),
            "-o".into(),
            paths.converted.as_os_str().to_owned(),
        ],
        loss,
    )
    .await?;
    require_nonempty(&paths.converted).await?;

    run_tool(
        &tools.dovi_tool,
        &[
            "extract-rpu".into(),
            "-i".into(),
            paths.raw.as_os_str().to_owned(),
            "-o".into(),
            paths.rpu.as_os_str().to_owned(),
        ],
        loss,
    )
    .await?;
    require_nonempty(&paths.rpu).await?;
    let summary = run_tool(
        &tools.dovi_tool,
        &[
            "info".into(),
            "-i".into(),
            paths.rpu.as_os_str().to_owned(),
            "--summary".into(),
        ],
        loss,
    )
    .await?;
    let el_type = parse_el_type(&summary);

    run_tool(
        &tools.mkvmerge,
        &[
            "-o".into(),
            paths.replacement.as_os_str().to_owned(),
            paths.converted.as_os_str().to_owned(),
            "--no-video".into(),
            file.path.as_os_str().to_owned(),
        ],
        loss,
    )
    .await?;
    require_nonempty(&paths.replacement).await?;

    let source_probe = plurx_core::scan::probe::probe(&file.path)
        .await
        .map_err(|error| format!("probing source before commit: {error}"))?;
    let replacement_probe = plurx_core::scan::probe::probe(&paths.replacement)
        .await
        .map_err(|error| format!("probing replacement before commit: {error}"))?;
    verify_replacement(&source_probe, &replacement_probe)?;
    let bytes_after = i64::try_from(
        tokio::fs::metadata(&paths.replacement)
            .await
            .map_err(|error| format!("stat {}: {error}", paths.replacement.display()))?
            .len(),
    )
    .map_err(|_| "replacement is too large to record".to_owned())?;
    Ok(VerifiedReplacement {
        el_type,
        bytes_after,
    })
}

pub async fn verify_existing(
    file: &MediaFile,
    el_type: Option<&'static str>,
) -> Result<VerifiedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    let source_exists = tokio::fs::try_exists(&file.path)
        .await
        .map_err(|error| error.to_string())?;
    let staged_exists = tokio::fs::try_exists(&paths.staged_original)
        .await
        .map_err(|error| error.to_string())?;
    let replacement_exists = tokio::fs::try_exists(&paths.replacement)
        .await
        .map_err(|error| error.to_string())?;
    let (source_path, replacement_path) = if staged_exists && source_exists && !replacement_exists {
        (&paths.staged_original, &file.path)
    } else if source_exists && replacement_exists {
        (&file.path, &paths.replacement)
    } else if staged_exists && replacement_exists {
        (&paths.staged_original, &paths.replacement)
    } else {
        return Err("verified conversion artifacts cannot be recovered".to_owned());
    };
    let source_probe = plurx_core::scan::probe::probe(source_path)
        .await
        .map_err(|error| format!("probing original for recovery: {error}"))?;
    let replacement_probe = plurx_core::scan::probe::probe(replacement_path)
        .await
        .map_err(|error| format!("probing replacement for recovery: {error}"))?;
    verify_replacement(&source_probe, &replacement_probe)?;
    let bytes_after = i64::try_from(
        tokio::fs::metadata(replacement_path)
            .await
            .map_err(|error| format!("stat {}: {error}", replacement_path.display()))?
            .len(),
    )
    .map_err(|_| "replacement is too large to record".to_owned())?;
    Ok(VerifiedReplacement {
        el_type,
        bytes_after,
    })
}

async fn prepare_fresh_directory(paths: &ConversionPaths, file: &MediaFile) -> Result<(), String> {
    if tokio::fs::try_exists(&paths.staged_original)
        .await
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "conversion recovery required: staged original remains at {}",
            paths.staged_original.display()
        ));
    }
    if tokio::fs::try_exists(&file.path)
        .await
        .map_err(|error| error.to_string())?
    {
        match tokio::fs::remove_dir_all(&paths.directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cleaning {}: {error}", paths.directory.display())),
        }
        tokio::fs::create_dir(&paths.directory)
            .await
            .map_err(|error| format!("creating {}: {error}", paths.directory.display()))?;
        return Ok(());
    }
    Err("source disappeared before conversion started".to_owned())
}

pub async fn publish_verified(
    file: &MediaFile,
    keep_original: bool,
    loss: &CancellationToken,
    expected_object_version: Option<&str>,
) -> Result<PublishedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before publication".to_owned());
    }
    if keep_original
        && tokio::fs::try_exists(&paths.retained_original)
            .await
            .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "refusing to overwrite retained original {}",
            paths.retained_original.display()
        ));
    }

    let source_exists = tokio::fs::try_exists(&file.path)
        .await
        .map_err(|error| error.to_string())?;
    let staged_exists = tokio::fs::try_exists(&paths.staged_original)
        .await
        .map_err(|error| error.to_string())?;
    let replacement_exists = tokio::fs::try_exists(&paths.replacement)
        .await
        .map_err(|error| error.to_string())?;

    if source_exists && !staged_exists {
        let expected_object_version = expected_object_version
            .ok_or_else(|| "source identity was not captured before publication".to_owned())?;
        let fence =
            crate::fragment_index_cluster::open_source_fence(file, Some(expected_object_version))
                .await?;
        if loss.is_cancelled() || !fence.unchanged() {
            return Err("source changed before publication".to_owned());
        }
        if !replacement_exists {
            return Err("verified replacement is missing".to_owned());
        }
        tokio::fs::rename(&file.path, &paths.staged_original)
            .await
            .map_err(|error| format!("staging original: {error}"))?;
    } else if !source_exists && !staged_exists {
        return Err("source and staged original are both missing".to_owned());
    }

    if !tokio::fs::try_exists(&file.path)
        .await
        .map_err(|error| error.to_string())?
    {
        if !tokio::fs::try_exists(&paths.replacement)
            .await
            .map_err(|error| error.to_string())?
        {
            // The original is recoverable, so put its public pathname back.
            tokio::fs::rename(&paths.staged_original, &file.path)
                .await
                .map_err(|error| {
                    format!("restoring original after missing replacement: {error}")
                })?;
            return Err("verified replacement disappeared before publication".to_owned());
        }
        if loss.is_cancelled() {
            tokio::fs::rename(&paths.staged_original, &file.path)
                .await
                .map_err(|error| format!("restoring original after lease loss: {error}"))?;
            return Err("conversion lease was lost before replacement rename".to_owned());
        }
        tokio::fs::rename(&paths.replacement, &file.path)
            .await
            .map_err(|error| format!("publishing replacement: {error}"))?;
    }

    let probe = plurx_core::scan::probe::probe(&file.path)
        .await
        .map_err(|error| format!("re-probing published replacement: {error}"))?;
    if probe.dolby_vision.profile != Some(8) || probe.dolby_vision.el_present != Some(false) {
        return Err(
            "published path does not contain the verified Profile 8 replacement".to_owned(),
        );
    }
    let metadata = tokio::fs::metadata(&file.path)
        .await
        .map_err(|error| format!("stat published replacement: {error}"))?;
    let size = i64::try_from(metadata.len()).map_err(|_| "replacement is too large".to_owned())?;
    let mtime = modified_seconds(&metadata);

    let original_path = if keep_original {
        if tokio::fs::try_exists(&paths.staged_original)
            .await
            .map_err(|error| error.to_string())?
        {
            tokio::fs::rename(&paths.staged_original, &paths.retained_original)
                .await
                .map_err(|error| format!("retaining original: {error}"))?;
        }
        Some(paths.retained_original.display().to_string())
    } else {
        match tokio::fs::remove_file(&paths.staged_original).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("deleting staged original: {error}")),
        }
        None
    };
    Ok(PublishedReplacement {
        original_path,
        bytes_after: size,
        probe,
        size,
        mtime,
    })
}

pub async fn recover_published(
    file: &MediaFile,
    expected_bytes: i64,
) -> Result<PublishedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    let metadata = tokio::fs::metadata(&file.path)
        .await
        .map_err(|error| format!("stat published replacement: {error}"))?;
    let size = i64::try_from(metadata.len()).map_err(|_| "replacement is too large".to_owned())?;
    if size != expected_bytes {
        return Err(format!(
            "published replacement size is {size}, verified ledger recorded {expected_bytes}"
        ));
    }
    let probe = plurx_core::scan::probe::probe(&file.path)
        .await
        .map_err(|error| format!("re-probing published replacement: {error}"))?;
    if probe.dolby_vision.profile != Some(8) || probe.dolby_vision.el_present != Some(false) {
        return Err(
            "published path does not contain the verified Profile 8 replacement".to_owned(),
        );
    }
    let original_path = tokio::fs::try_exists(&paths.retained_original)
        .await
        .map_err(|error| error.to_string())?
        .then(|| paths.retained_original.display().to_string());
    Ok(PublishedReplacement {
        original_path,
        bytes_after: size,
        probe,
        size,
        mtime: modified_seconds(&metadata),
    })
}

pub async fn cleanup_after_failure(file: &MediaFile) {
    let Ok(paths) = ConversionPaths::for_file(file) else {
        return;
    };
    if tokio::fs::try_exists(&paths.staged_original)
        .await
        .unwrap_or(true)
    {
        // A staged original is crash-recovery state, never scratch.
        return;
    }
    let _ = tokio::fs::remove_dir_all(paths.directory).await;
}

pub async fn cleanup_after_commit(file: &MediaFile) {
    let Ok(paths) = ConversionPaths::for_file(file) else {
        return;
    };
    if !tokio::fs::try_exists(&paths.staged_original)
        .await
        .unwrap_or(true)
    {
        let _ = tokio::fs::remove_dir_all(paths.directory).await;
    }
}

async fn run_tool(
    program: &str,
    args: &[OsString],
    loss: &CancellationToken,
) -> Result<String, String> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("starting {program}: {error}"))?;
    let output = tokio::select! {
        output = child.wait_with_output() => output.map_err(|error| format!("waiting for {program}: {error}"))?,
        () = loss.cancelled() => return Err(format!("{program} cancelled after conversion lease loss")),
    };
    let stdout = bounded_text(&output.stdout);
    let stderr = bounded_text(&output.stderr);
    if !output.status.success() {
        let detail = [stderr.as_str(), stdout.as_str()]
            .into_iter()
            .find(|text| !text.trim().is_empty())
            .unwrap_or("no diagnostic output")
            .trim();
        return Err(format!(
            "{program} exited with {}: {detail}",
            output
                .status
                .code()
                .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
        ));
    }
    Ok(if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    })
}

fn bounded_text(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(MAX_TOOL_OUTPUT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

async fn require_nonempty(path: &Path) -> Result<(), String> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("{} is not a non-empty file", path.display()));
    }
    Ok(())
}

pub fn parse_el_type(summary: &str) -> Option<&'static str> {
    let upper = summary.to_ascii_uppercase();
    if upper
        .lines()
        .any(|line| line.contains("PROFILE:") && line.contains('7') && line.contains("(MEL)"))
    {
        Some("mel")
    } else if upper
        .lines()
        .any(|line| line.contains("PROFILE:") && line.contains('7') && line.contains("(FEL)"))
    {
        Some("fel")
    } else {
        None
    }
}

pub fn verify_replacement(source: &ProbeResult, replacement: &ProbeResult) -> Result<(), String> {
    if replacement.dolby_vision.profile != Some(8) {
        return Err(format!(
            "replacement dv_profile is {:?}, expected 8",
            replacement.dolby_vision.profile
        ));
    }
    if replacement.dolby_vision.el_present != Some(false) {
        return Err(format!(
            "replacement el_present_flag is {:?}, expected 0",
            replacement.dolby_vision.el_present
        ));
    }
    if source.audio_streams.len() != replacement.audio_streams.len() {
        return Err(format!(
            "audio track count changed from {} to {}",
            source.audio_streams.len(),
            replacement.audio_streams.len()
        ));
    }
    if source.subtitle_streams.len() != replacement.subtitle_streams.len() {
        return Err(format!(
            "subtitle track count changed from {} to {}",
            source.subtitle_streams.len(),
            replacement.subtitle_streams.len()
        ));
    }
    let source_chapters = chapter_count(source)?;
    let replacement_chapters = chapter_count(replacement)?;
    if source_chapters != replacement_chapters {
        return Err(format!(
            "chapter count changed from {source_chapters} to {replacement_chapters}"
        ));
    }
    let source_duration = source
        .duration_ms
        .ok_or_else(|| "source duration is unknown".to_owned())?;
    let replacement_duration = replacement
        .duration_ms
        .ok_or_else(|| "replacement duration is unknown".to_owned())?;
    let frame_ms = frame_duration_ms(source)?;
    let drift = source_duration.abs_diff(replacement_duration) as f64;
    if drift > frame_ms + 0.001 {
        return Err(format!(
            "duration changed by {drift:.3} ms, more than one {frame_ms:.3} ms frame"
        ));
    }
    Ok(())
}

fn raw_probe(probe: &ProbeResult) -> Result<serde_json::Value, String> {
    serde_json::from_str(
        probe
            .raw_json
            .as_deref()
            .ok_or_else(|| "probe JSON is unavailable".to_owned())?,
    )
    .map_err(|error| format!("invalid probe JSON: {error}"))
}

fn chapter_count(probe: &ProbeResult) -> Result<usize, String> {
    raw_probe(probe)?
        .get("chapters")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| "probe did not report a chapter array".to_owned())
}

fn frame_duration_ms(probe: &ProbeResult) -> Result<f64, String> {
    let raw = raw_probe(probe)?;
    let stream = raw
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
                    && stream
                        .get("disposition")
                        .and_then(|value| value.get("attached_pic"))
                        .and_then(serde_json::Value::as_i64)
                        != Some(1)
            })
        })
        .ok_or_else(|| "probe did not report a video stream".to_owned())?;
    for key in ["avg_frame_rate", "r_frame_rate"] {
        let Some(rate) = stream.get(key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if let Some(fps) = parse_rate(rate).filter(|fps| *fps > 0.0) {
            return Ok(1000.0 / fps);
        }
    }
    Err("source frame rate is unknown".to_owned())
}

fn parse_rate(rate: &str) -> Option<f64> {
    let (numerator, denominator) = rate.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    (denominator != 0.0).then_some(numerator / denominator)
}

#[cfg(unix)]
fn modified_seconds(metadata: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime()
}

#[cfg(not(unix))]
fn modified_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use plurx_core::domain::{AudioStream, DolbyVisionFacts, SubtitleStream};

    use super::*;

    fn probe(profile: i64, el: bool, duration_ms: i64, chapters: usize) -> ProbeResult {
        ProbeResult {
            duration_ms: Some(duration_ms),
            dolby_vision: DolbyVisionFacts {
                profile: Some(profile),
                el_present: Some(el),
                ..Default::default()
            },
            audio_streams: vec![AudioStream {
                index: 0,
                codec: "truehd".to_owned(),
                channels: Some(8),
                language: Some("eng".to_owned()),
                title: None,
                default: true,
            }],
            subtitle_streams: vec![SubtitleStream {
                index: 0,
                codec: "hdmv_pgs_subtitle".to_owned(),
                language: Some("eng".to_owned()),
                title: None,
                default: true,
                forced: false,
                hearing_impaired: false,
            }],
            raw_json: Some(
                serde_json::json!({
                    "streams": [{
                        "codec_type": "video",
                        "avg_frame_rate": "24000/1001",
                        "disposition": { "attached_pic": 0 }
                    }],
                    "chapters": vec![serde_json::json!({}); chapters]
                })
                .to_string(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn el_summary_is_defensive_and_never_guesses() {
        assert_eq!(parse_el_type("Profile: 7 (MEL)"), Some("mel"));
        assert_eq!(parse_el_type("profile: 7 (fel)"), Some("fel"));
        assert_eq!(parse_el_type("Profile: 8.1"), None);
        assert_eq!(parse_el_type("MEL compatible"), None);
    }

    #[test]
    fn verification_accepts_one_frame_and_rejects_truncation() {
        let source = probe(7, true, 7_200_000, 12);
        let replacement = probe(8, false, 7_199_959, 12);
        verify_replacement(&source, &replacement).expect("within one 23.976 fps frame");

        let truncated = probe(8, false, 7_190_000, 12);
        assert!(verify_replacement(&source, &truncated)
            .expect_err("truncated output")
            .contains("more than one"));
    }

    #[test]
    fn verification_rejects_every_mux_mismatch() {
        let source = probe(7, true, 1_000, 1);
        let mut replacement = probe(8, false, 1_000, 1);
        replacement.audio_streams.clear();
        assert!(verify_replacement(&source, &replacement)
            .expect_err("missing audio")
            .contains("audio track"));

        let mut replacement = probe(8, false, 1_000, 1);
        replacement.subtitle_streams.clear();
        assert!(verify_replacement(&source, &replacement)
            .expect_err("missing subtitle")
            .contains("subtitle track"));

        let replacement = probe(8, false, 1_000, 0);
        assert!(verify_replacement(&source, &replacement)
            .expect_err("missing chapter")
            .contains("chapter count"));
    }

    #[test]
    fn mkvmerge_version_floor_is_parsed_from_the_supported_banner() {
        assert_eq!(
            mkvmerge_major("mkvmerge v68.0.0 ('The Curtain') 64-bit"),
            Some(68)
        );
        assert_eq!(mkvmerge_major("mkvmerge v100.1.2"), Some(100));
        assert_eq!(mkvmerge_major("unknown"), None);
    }

    #[tokio::test]
    async fn a_missing_tool_is_named_before_queue_work_can_start() {
        let missing = crate::test_temp_path(format!("missing-dovi-{}", uuid::Uuid::new_v4()));
        let tools = DvDiskTools::new(
            "ffmpeg".to_owned(),
            missing.display().to_string(),
            missing.display().to_string(),
        );
        let capabilities = probe_capabilities_with(&tools).await;
        assert!(!capabilities.available);
        assert!(capabilities
            .unavailable_reason()
            .expect("reason")
            .contains("dovi_tool"));
    }
}
