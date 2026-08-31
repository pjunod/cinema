//! Permanent Dolby Vision Profile 7 to 8.1 conversion.
//!
//! This is intentionally separate from the playback copy pipe. The output is
//! proved as a complete replacement before the source pathname moves, and the
//! scanner then sees an ordinary Profile 8 file on its next read.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use plurx_core::domain::{MediaFile, ProbeResult};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_OUTPUT: usize = 32 * 1024;
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const SCRATCH_OWNER_FILE: &str = ".plurx-dv-owner.json";
const SCRATCH_OWNER_VERSION: u32 = 1;
const MAX_SCRATCH_OWNER_BYTES: u64 = 32 * 1024;
const MAX_SCRATCH_ENTRIES: usize = 16;

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
    let mut dovi_tool = probe_tool(&tools.dovi_tool).await;
    if dovi_tool.available
        && dovi_tool
            .version
            .as_deref()
            .and_then(dovi_tool_version)
            .is_none_or(|version| version < (2, 3, 3))
    {
        dovi_tool.available = false;
        dovi_tool.reason = Some("dovi_tool 2.3.3 or newer is required".to_owned());
    }
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
    let output = bounded_tool_probe(command).await;
    match output {
        Ok((status, stdout, stderr)) if status.success() => {
            let text = if stdout.is_empty() { &stderr } else { &stdout };
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
        Ok((status, _, _)) => ToolCapability {
            command: command.to_owned(),
            version: None,
            available: false,
            reason: Some(format!(
                "{command} --version exited with {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
            )),
        },
        Err(error) => ToolCapability {
            command: command.to_owned(),
            version: None,
            available: false,
            reason: Some(format!("{command} version probe failed: {error}")),
        },
    }
}

async fn bounded_tool_probe(
    command: &str,
) -> io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = tokio::process::Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("tool probe stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("tool probe stderr was not piped"))?;
    let probe = async move {
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded_output(stdout),
            read_bounded_output(stderr),
            child.wait(),
        )?;
        Ok((status, stdout, stderr))
    };
    tokio::time::timeout(TOOL_PROBE_TIMEOUT, probe)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "tool version probe timed out"))?
}

async fn read_bounded_output<R: tokio::io::AsyncRead + Unpin>(reader: R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_TOOL_OUTPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_TOOL_OUTPUT {
        return Err(io::Error::other(format!(
            "tool version output exceeded {MAX_TOOL_OUTPUT} bytes"
        )));
    }
    Ok(bytes)
}

fn mkvmerge_major(version: &str) -> Option<u64> {
    let marker = version.find("mkvmerge v")? + "mkvmerge v".len();
    version[marker..].split('.').next()?.parse().ok()
}

fn dovi_tool_version(version: &str) -> Option<(u64, u64, u64)> {
    version.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v');
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts
            .next()?
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        Some((major, minor, patch))
    })
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
    directory_name: String,
    pub raw: PathBuf,
    pub converted: PathBuf,
    pub rpu: PathBuf,
    pub replacement: PathBuf,
    pub staged_original: PathBuf,
    pub retained_original: PathBuf,
}

impl ConversionPaths {
    pub fn for_file(file: &MediaFile) -> Result<Self, String> {
        let path = file
            .path
            .to_str()
            .ok_or_else(|| "source path is not valid UTF-8".to_owned())?;
        let parent = file
            .path
            .parent()
            .ok_or_else(|| "source has no parent directory".to_owned())?;
        let name = file
            .path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| "source filename is not valid UTF-8".to_owned())?;
        let directory_name = format!(".{name}.plurx-dv-{}", file.id);
        let directory = parent.join(&directory_name);
        Ok(Self {
            raw: directory.join("BL_RPU.hevc"),
            converted: directory.join("BL_RPU.p81.hevc"),
            rpu: directory.join("RPU.bin"),
            replacement: directory.join("replacement.mkv"),
            staged_original: directory.join("source.p7.original"),
            retained_original: PathBuf::from(format!("{path}.p7.orig")),
            directory,
            directory_name,
        })
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ScratchOwner {
    version: u32,
    file_id: i64,
    source_path: String,
    source_object_version: String,
}

impl ScratchOwner {
    fn new(file: &MediaFile, source_object_version: String) -> Result<Self, String> {
        Ok(Self {
            version: SCRATCH_OWNER_VERSION,
            file_id: file.id,
            source_path: file
                .path
                .to_str()
                .ok_or_else(|| "source path is not valid UTF-8".to_owned())?
                .to_owned(),
            source_object_version,
        })
    }

    fn owns_file(&self, file: &MediaFile) -> bool {
        self.version == SCRATCH_OWNER_VERSION
            && self.file_id == file.id
            && file.path.to_str() == Some(self.source_path.as_str())
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
    require_owned_scratch(&paths, file, None).await?;
    let source_exists = path_entry_exists(&file.path).await?;
    let staged_exists = path_entry_exists(&paths.staged_original).await?;
    let replacement_exists = path_entry_exists(&paths.replacement).await?;
    let (source_path, replacement_path) = if staged_exists && source_exists && replacement_exists {
        return Err(
            "source, staged original, and replacement all exist; refusing ambiguous recovery"
                .to_owned(),
        );
    } else if staged_exists && source_exists && !replacement_exists {
        (&paths.staged_original, &file.path)
    } else if source_exists && replacement_exists && !staged_exists {
        (&file.path, &paths.replacement)
    } else if staged_exists && replacement_exists && !source_exists {
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
    let source_object_version = current_source_object_version(file).await?;
    if path_entry_exists(&paths.directory).await? {
        let directory = require_owned_scratch(paths, file, Some(&source_object_version)).await?;
        match directory.child_metadata("source.p7.original").await {
            Ok(_) => {
                return Err(format!(
                    "conversion recovery required: staged original remains at {}",
                    paths.staged_original.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspecting staged original in {}: {error}",
                    paths.directory.display()
                ));
            }
        }
        remove_owned_scratch(paths, file, Some(&source_object_version)).await?;
    }
    create_owned_scratch(paths, &ScratchOwner::new(file, source_object_version)?).await
}

async fn current_source_object_version(file: &MediaFile) -> Result<String, String> {
    let metadata = tokio::fs::metadata(&file.path)
        .await
        .map_err(|error| format!("stat {}: {error}", file.path.display()))?;
    if !metadata.is_file() {
        return Err("source is not a regular file".to_owned());
    }
    if metadata.len() != file.size.max(0) as u64 || modified_seconds(&metadata) != file.mtime {
        return Err("source no longer matches the scanner's size/mtime identity".to_owned());
    }
    metadata_object_version(&metadata)
}

#[cfg(unix)]
fn metadata_object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(not(unix))]
fn metadata_object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
    let modified = metadata
        .modified()
        .map_err(|error| format!("reading source modification time: {error}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("source modification time precedes unix epoch: {error}"))?;
    Ok(format!("{}:{}", metadata.len(), modified.as_nanos()))
}

async fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

async fn require_owned_scratch(
    paths: &ConversionPaths,
    file: &MediaFile,
    expected_source_version: Option<&str>,
) -> Result<plurx_core::fs_secure::SecureDirectory, String> {
    let directory = plurx_core::fs_secure::SecureDirectory::open(&paths.directory)
        .await
        .map_err(|error| format!("opening conversion scratch safely: {error}"))?;
    let bytes = directory
        .read_bounded_child(SCRATCH_OWNER_FILE, MAX_SCRATCH_OWNER_BYTES)
        .await
        .map_err(|error| {
            format!(
                "refusing unowned conversion scratch {}: {error}",
                paths.directory.display()
            )
        })?;
    let owner: ScratchOwner = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "refusing invalid conversion scratch owner {}: {error}",
            paths.directory.display()
        )
    })?;
    if !owner.owns_file(file)
        || expected_source_version.is_some_and(|expected| owner.source_object_version != expected)
    {
        return Err(format!(
            "refusing conversion scratch not owned by this exact source: {}",
            paths.directory.display()
        ));
    }
    Ok(directory)
}

async fn create_owned_scratch(paths: &ConversionPaths, owner: &ScratchOwner) -> Result<(), String> {
    let bytes = serde_json::to_vec(owner)
        .map_err(|error| format!("encoding conversion scratch owner: {error}"))?;
    if bytes.len() as u64 > MAX_SCRATCH_OWNER_BYTES {
        return Err("conversion scratch owner exceeds its bounded format".to_owned());
    }
    let scratch_path = paths.directory.clone();
    tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(scratch_path)
    })
    .await
    .map_err(|error| format!("joining conversion scratch creation: {error}"))?
    .map_err(|error| format!("creating {}: {error}", paths.directory.display()))?;
    let directory = plurx_core::fs_secure::SecureDirectory::open(&paths.directory)
        .await
        .map_err(|error| format!("opening new conversion scratch safely: {error}"))?;
    directory
        .atomic_write_child(SCRATCH_OWNER_FILE, &bytes)
        .await
        .map_err(|error| format!("writing conversion scratch owner: {error}"))?;
    sync_directory(
        paths
            .directory
            .parent()
            .ok_or_else(|| "conversion scratch has no parent directory".to_owned())?,
    )
    .await
}

async fn remove_owned_scratch(
    paths: &ConversionPaths,
    file: &MediaFile,
    expected_source_version: Option<&str>,
) -> Result<(), String> {
    if !path_entry_exists(&paths.directory).await? {
        return Ok(());
    }
    let directory = require_owned_scratch(paths, file, expected_source_version).await?;
    let identity = directory
        .identity()
        .await
        .map_err(|error| format!("identifying conversion scratch: {error}"))?;
    let parent_path = paths
        .directory
        .parent()
        .ok_or_else(|| "conversion scratch has no parent directory".to_owned())?;
    let parent = plurx_core::fs_secure::SecureDirectory::open(parent_path)
        .await
        .map_err(|error| format!("opening conversion scratch parent safely: {error}"))?;
    let quarantine = format!(".plurx-dv-cleanup-{}", uuid::Uuid::new_v4().simple());
    if !parent
        .rename_child_noreplace(&paths.directory_name, &quarantine)
        .await
        .map_err(|error| format!("quarantining conversion scratch: {error}"))?
    {
        return Err("conversion scratch quarantine destination unexpectedly exists".to_owned());
    }
    let quarantined = parent
        .open_child_directory(&quarantine)
        .await
        .map_err(|error| format!("opening quarantined conversion scratch: {error}"))?;
    let quarantined_identity = quarantined
        .identity()
        .await
        .map_err(|error| format!("identifying quarantined conversion scratch: {error}"))?;
    if !quarantined_identity.same_inode(identity) {
        let _ = parent
            .rename_child_noreplace(&quarantine, &paths.directory_name)
            .await;
        return Err("conversion scratch changed while it was quarantined".to_owned());
    }
    parent
        .remove_child_tree(&quarantine, MAX_SCRATCH_ENTRIES, 1)
        .await
        .map_err(|error| format!("removing owned conversion scratch: {error}"))
}

async fn sync_regular_file(path: &Path) -> Result<(), String> {
    let file = plurx_core::fs_secure::open_read_nofollow(path)
        .await
        .map_err(|error| format!("opening {} for durable sync: {error}", path.display()))?;
    file.sync_all()
        .await
        .map_err(|error| format!("syncing {}: {error}", path.display()))
}

async fn sync_directory(path: &Path) -> Result<(), String> {
    let directory = plurx_core::fs_secure::SecureDirectory::open(path)
        .await
        .map_err(|error| format!("opening directory {} for sync: {error}", path.display()))?;
    tokio::task::spawn_blocking(move || {
        if unsafe { libc::fsync(directory.raw_fd()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })
    .await
    .map_err(|error| format!("joining directory sync: {error}"))?
    .map_err(|error| format!("syncing directory {}: {error}", path.display()))
}

async fn rename_noreplace_durable(from: &Path, to: &Path) -> Result<bool, String> {
    let from_parent_path = from
        .parent()
        .ok_or_else(|| "rename source has no parent directory".to_owned())?;
    let to_parent_path = to
        .parent()
        .ok_or_else(|| "rename destination has no parent directory".to_owned())?;
    let from_parent = plurx_core::fs_secure::SecureDirectory::open(from_parent_path)
        .await
        .map_err(|error| format!("opening rename source parent safely: {error}"))?;
    let to_parent = plurx_core::fs_secure::SecureDirectory::open(to_parent_path)
        .await
        .map_err(|error| format!("opening rename destination parent safely: {error}"))?;
    let from_name = from
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("rename source is not valid UTF-8: {}", from.display()))?
        .to_owned();
    let to_name = to
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("rename destination is not valid UTF-8: {}", to.display()))?
        .to_owned();
    let renamed = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let from = CString::new(from_name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename source has NUL"))?;
        let to = CString::new(to_name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "rename destination has NUL")
        })?;
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                from_parent.raw_fd(),
                from.as_ptr(),
                to_parent.raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from_parent.raw_fd(),
                from.as_ptr(),
                to_parent.raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            ) as i32
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = -1;
        if result == 0 {
            if unsafe { libc::fsync(from_parent.raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if from_parent.raw_fd() != to_parent.raw_fd()
                && unsafe { libc::fsync(to_parent.raw_fd()) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            return Ok(true);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive rename is unsupported on this platform",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(false)
            } else {
                Err(error)
            }
        }
    })
    .await
    .map_err(|error| format!("joining exclusive rename: {error}"))?
    .map_err(|error| format!("renaming {} to {}: {error}", from.display(), to.display()))?;
    Ok(renamed)
}

#[cfg(unix)]
fn same_source_inode(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.size() == after.size()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(not(unix))]
fn same_source_inode(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

async fn finalize_staged_original(
    paths: &ConversionPaths,
    keep_original: bool,
) -> Result<Option<String>, String> {
    let staged_exists = path_entry_exists(&paths.staged_original).await?;
    if keep_original {
        if staged_exists
            && !rename_noreplace_durable(&paths.staged_original, &paths.retained_original).await?
        {
            return Err(format!(
                "refusing to overwrite retained original {}",
                paths.retained_original.display()
            ));
        }
        if !path_entry_exists(&paths.retained_original).await? {
            return Err("retained original is missing after publication".to_owned());
        }
        Ok(Some(
            paths
                .retained_original
                .to_str()
                .ok_or_else(|| "retained original path is not valid UTF-8".to_owned())?
                .to_owned(),
        ))
    } else {
        if staged_exists {
            tokio::fs::remove_file(&paths.staged_original)
                .await
                .map_err(|error| format!("deleting staged original: {error}"))?;
            sync_directory(&paths.directory).await?;
            return Ok(None);
        }
        // A prior attempt may already have completed retention before dying.
        // Never reinterpret that durable safety copy as deletable merely
        // because the global setting changed during recovery.
        Ok(path_entry_exists(&paths.retained_original).await?.then(|| {
            paths
                .retained_original
                .to_str()
                .expect("ConversionPaths refused non-UTF-8")
                .to_owned()
        }))
    }
}

async fn rollback_published(file: &MediaFile, paths: &ConversionPaths) -> Result<(), String> {
    if !path_entry_exists(&paths.staged_original).await? {
        return Err(
            "published replacement failed validation and no staged original remains".to_owned(),
        );
    }
    let quarantine = paths.directory.join(format!(
        "failed-published-{}.mkv",
        uuid::Uuid::new_v4().simple()
    ));
    if !rename_noreplace_durable(&file.path, &quarantine).await? {
        return Err("failed replacement quarantine destination unexpectedly exists".to_owned());
    }
    if !rename_noreplace_durable(&paths.staged_original, &file.path).await? {
        return Err(format!(
            "a newer source appeared while restoring the staged original; failed output remains at {}",
            quarantine.display()
        ));
    }
    tokio::fs::remove_file(&quarantine)
        .await
        .map_err(|error| format!("removing rolled-back replacement: {error}"))?;
    sync_directory(&paths.directory).await
}

async fn discard_stale_replacement(paths: &ConversionPaths) -> Result<(), String> {
    if path_entry_exists(&paths.replacement).await? {
        tokio::fs::remove_file(&paths.replacement)
            .await
            .map_err(|error| format!("discarding replacement for a raced source: {error}"))?;
        sync_directory(&paths.directory).await?;
    }
    Ok(())
}

async fn published_probe_and_metadata(
    file: &MediaFile,
) -> Result<(ProbeResult, std::fs::Metadata), String> {
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
    Ok((probe, metadata))
}

pub async fn publish_verified(
    file: &MediaFile,
    keep_original: bool,
    loss: &CancellationToken,
    expected_object_version: Option<&str>,
) -> Result<PublishedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    require_owned_scratch(&paths, file, None).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before publication".to_owned());
    }
    if keep_original && path_entry_exists(&paths.retained_original).await? {
        return Err(format!(
            "refusing to overwrite retained original {}",
            paths.retained_original.display()
        ));
    }

    let source_exists = path_entry_exists(&file.path).await?;
    let staged_exists = path_entry_exists(&paths.staged_original).await?;
    let replacement_exists = path_entry_exists(&paths.replacement).await?;
    if source_exists && staged_exists && replacement_exists {
        return Err(
            "source, staged original, and replacement all exist; refusing ambiguous publication"
                .to_owned(),
        );
    }

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
        // A successful probe only proves page-cache bytes. Persist the exact
        // replacement before the first operation that moves the source.
        sync_regular_file(&paths.replacement).await?;
        if loss.is_cancelled() || !fence.unchanged() {
            return Err("source changed while the replacement was made durable".to_owned());
        }
        let fenced_metadata = fence
            .handle
            .metadata()
            .map_err(|error| format!("re-fstat fenced source: {error}"))?;
        if !rename_noreplace_durable(&file.path, &paths.staged_original).await? {
            return Err("refusing to overwrite an existing staged original".to_owned());
        }
        let staged_metadata = tokio::fs::metadata(&paths.staged_original)
            .await
            .map_err(|error| format!("stat staged original: {error}"))?;
        if !same_source_inode(&fenced_metadata, &staged_metadata) {
            let restored = rename_noreplace_durable(&paths.staged_original, &file.path).await?;
            let discarded = discard_stale_replacement(&paths).await;
            let mut error = if restored {
                "source pathname changed during staging; restored the raced source".to_owned()
            } else {
                "source pathname changed during staging and a newer public source prevented restoration"
                    .to_owned()
            };
            if let Err(discard) = discarded {
                error.push_str(&format!("; stale replacement cleanup failed: {discard}"));
            }
            return Err(error);
        }
    } else if !source_exists && !staged_exists {
        return Err("source and staged original are both missing".to_owned());
    }

    if !path_entry_exists(&file.path).await? {
        if !path_entry_exists(&paths.replacement).await? {
            // The original is recoverable, so put its public pathname back.
            if !rename_noreplace_durable(&paths.staged_original, &file.path).await? {
                return Err(
                    "a newer public source appeared while restoring a missing replacement"
                        .to_owned(),
                );
            }
            return Err("verified replacement disappeared before publication".to_owned());
        }
        if loss.is_cancelled() {
            if !rename_noreplace_durable(&paths.staged_original, &file.path).await? {
                return Err(
                    "conversion lease was lost and a newer source prevented restoration".to_owned(),
                );
            }
            return Err("conversion lease was lost before replacement rename".to_owned());
        }
        sync_regular_file(&paths.replacement).await?;
        if !rename_noreplace_durable(&paths.replacement, &file.path).await? {
            return Err("a newer source appeared before replacement publication".to_owned());
        }
    }

    let (probe, metadata) = match published_probe_and_metadata(file).await {
        Ok(published) => published,
        Err(error) => {
            let rollback = rollback_published(file, &paths).await;
            return Err(match rollback {
                Ok(()) => format!("{error}; restored the staged original"),
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            });
        }
    };
    let size = i64::try_from(metadata.len()).map_err(|_| "replacement is too large".to_owned())?;
    let mtime = modified_seconds(&metadata);
    if let Err(error) = sync_regular_file(&file.path).await {
        let rollback = rollback_published(file, &paths).await;
        return Err(match rollback {
            Ok(()) => format!("{error}; restored the staged original"),
            Err(rollback) => format!("{error}; rollback failed: {rollback}"),
        });
    }

    let original_path = match finalize_staged_original(&paths, keep_original).await {
        Ok(original_path) => original_path,
        Err(error) => {
            let rollback = rollback_published(file, &paths).await;
            return Err(match rollback {
                Ok(()) => format!("{error}; restored the staged original"),
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            });
        }
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
    keep_original: bool,
) -> Result<PublishedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    if path_entry_exists(&paths.directory).await? {
        require_owned_scratch(&paths, file, None).await?;
    }
    let (probe, metadata) = match published_probe_and_metadata(file).await {
        Ok(published) => published,
        Err(error) => {
            if path_entry_exists(&paths.staged_original).await? {
                let rollback = rollback_published(file, &paths).await;
                return Err(match rollback {
                    Ok(()) => format!("{error}; restored the staged original"),
                    Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                });
            }
            return Err(error);
        }
    };
    let size = i64::try_from(metadata.len()).map_err(|_| "replacement is too large".to_owned())?;
    if size != expected_bytes {
        let error = format!(
            "published replacement size is {size}, verified ledger recorded {expected_bytes}"
        );
        if path_entry_exists(&paths.staged_original).await? {
            let rollback = rollback_published(file, &paths).await;
            return Err(match rollback {
                Ok(()) => format!("{error}; restored the staged original"),
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            });
        }
        return Err(error);
    }
    let staged_original = if path_entry_exists(&paths.staged_original).await? {
        Some(paths.staged_original.as_path())
    } else if path_entry_exists(&paths.retained_original).await? {
        Some(paths.retained_original.as_path())
    } else {
        None
    };
    if let Some(original) = staged_original {
        let source_probe = plurx_core::scan::probe::probe(original)
            .await
            .map_err(|error| format!("probing original for published recovery: {error}"))?;
        if let Err(error) = verify_replacement(&source_probe, &probe) {
            if path_entry_exists(&paths.staged_original).await? {
                let rollback = rollback_published(file, &paths).await;
                return Err(match rollback {
                    Ok(()) => format!("{error}; restored the staged original"),
                    Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                });
            }
            return Err(error);
        }
    }
    sync_regular_file(&file.path).await?;
    let original_path = finalize_staged_original(&paths, keep_original).await?;
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
    if !path_entry_exists(&paths.directory).await.unwrap_or(true) {
        return;
    }
    let directory = match require_owned_scratch(&paths, file, None).await {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(%error, "refusing to clean unowned Dolby Vision scratch");
            return;
        }
    };
    match directory.child_metadata("source.p7.original").await {
        Ok(_) => {
            // A staged original is crash-recovery state, never scratch.
            return;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, "inspecting Dolby Vision scratch before cleanup");
            return;
        }
    }
    if let Err(error) = remove_owned_scratch(&paths, file, None).await {
        tracing::warn!(%error, "cleaning failed Dolby Vision conversion scratch");
    }
}

pub async fn cleanup_after_commit(file: &MediaFile) {
    let Ok(paths) = ConversionPaths::for_file(file) else {
        return;
    };
    if !path_entry_exists(&paths.directory).await.unwrap_or(true) {
        return;
    }
    let directory = match require_owned_scratch(&paths, file, None).await {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(%error, "refusing to clean unowned Dolby Vision scratch");
            return;
        }
    };
    match directory.child_metadata("source.p7.original").await {
        Ok(_) => return,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, "inspecting committed Dolby Vision scratch before cleanup");
            return;
        }
    }
    if let Err(error) = remove_owned_scratch(&paths, file, None).await {
        tracing::warn!(%error, "cleaning committed Dolby Vision conversion scratch");
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

    fn media_file(path: PathBuf) -> MediaFile {
        let metadata = std::fs::metadata(&path).expect("source metadata");
        MediaFile {
            id: 17,
            item_id: 3,
            path,
            size: metadata.len() as i64,
            mtime: modified_seconds(&metadata),
            duration_ms: Some(1_000),
            container: Some("mkv".to_owned()),
            video_codec: Some("hevc".to_owned()),
            video_profile: Some("Main 10".to_owned()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("dolby_vision".to_owned()),
            hdr_format: Some("Dolby Vision · Profile 7".to_owned()),
            dolby_vision: DolbyVisionFacts::default(),
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        }
    }

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

    #[test]
    fn dovi_tool_version_floor_is_parsed_defensively() {
        assert_eq!(dovi_tool_version("dovi_tool 2.3.3"), Some((2, 3, 3)));
        assert_eq!(
            dovi_tool_version("dovi_tool v2.4.0-nightly"),
            Some((2, 4, 0))
        );
        assert_eq!(dovi_tool_version("dovi_tool unknown"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capability_probe_rejects_an_old_dovi_tool() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::test_tempdir().expect("tool root");
        let dovi = root.path().join("dovi-tool");
        let mkvmerge = root.path().join("mkvmerge");
        std::fs::write(&dovi, "#!/bin/sh\necho 'dovi_tool 2.2.0'\n").expect("dovi script");
        std::fs::write(
            &mkvmerge,
            "#!/bin/sh\necho \"mkvmerge v74.0.0 ('You Oughta Know') 64-bit\"\n",
        )
        .expect("mkvmerge script");
        std::fs::set_permissions(&dovi, std::fs::Permissions::from_mode(0o700))
            .expect("dovi executable");
        std::fs::set_permissions(&mkvmerge, std::fs::Permissions::from_mode(0o700))
            .expect("mkvmerge executable");
        let tools = DvDiskTools::new(
            "ffmpeg".to_owned(),
            dovi.display().to_string(),
            mkvmerge.display().to_string(),
        );
        let capabilities = probe_capabilities_with(&tools).await;
        assert!(!capabilities.available);
        assert!(capabilities
            .unavailable_reason()
            .expect("reason")
            .contains("2.3.3 or newer"));
    }

    #[tokio::test]
    async fn capability_output_is_bounded_before_it_can_accumulate() {
        let bytes = vec![b'x'; MAX_TOOL_OUTPUT + 1];
        assert!(read_bounded_output(bytes.as_slice())
            .await
            .expect_err("oversized probe output")
            .to_string()
            .contains("exceeded"));
    }

    #[tokio::test]
    async fn exclusive_rename_never_clobbers_a_winning_destination() {
        let root = crate::test_tempdir().expect("rename root");
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        tokio::fs::write(&source, b"source")
            .await
            .expect("source bytes");
        tokio::fs::write(&destination, b"winner")
            .await
            .expect("destination bytes");
        assert!(!rename_noreplace_durable(&source, &destination)
            .await
            .expect("exclusive refusal"));
        assert_eq!(tokio::fs::read(&source).await.expect("source"), b"source");
        assert_eq!(
            tokio::fs::read(&destination).await.expect("destination"),
            b"winner"
        );

        tokio::fs::remove_file(&destination)
            .await
            .expect("remove winner");
        assert!(rename_noreplace_durable(&source, &destination)
            .await
            .expect("exclusive rename"));
        assert!(!path_entry_exists(&source).await.expect("source absence"));
        assert_eq!(
            tokio::fs::read(&destination).await.expect("destination"),
            b"source"
        );
    }

    #[tokio::test]
    async fn scratch_cleanup_requires_the_exact_owned_marker() {
        let root = crate::test_tempdir().expect("scratch root");
        let source = root.path().join("movie.mkv");
        tokio::fs::write(&source, b"profile seven")
            .await
            .expect("source bytes");
        let file = media_file(source);
        let paths = ConversionPaths::for_file(&file).expect("paths");
        prepare_fresh_directory(&paths, &file)
            .await
            .expect("first scratch");
        tokio::fs::write(&paths.raw, b"stale")
            .await
            .expect("stale scratch");
        prepare_fresh_directory(&paths, &file)
            .await
            .expect("owned scratch replacement");
        assert!(!path_entry_exists(&paths.raw).await.expect("stale removed"));

        tokio::fs::write(
            paths.directory.join(SCRATCH_OWNER_FILE),
            br#"{"version":1,"file_id":999,"source_path":"/other","source_object_version":"other"}"#,
        )
        .await
        .expect("foreign owner");
        tokio::fs::write(&paths.raw, b"must survive")
            .await
            .expect("foreign scratch bytes");
        assert!(prepare_fresh_directory(&paths, &file)
            .await
            .expect_err("foreign scratch refusal")
            .contains("refusing"));
        assert_eq!(
            tokio::fs::read(&paths.raw)
                .await
                .expect("preserved foreign data"),
            b"must survive"
        );
    }

    #[tokio::test]
    async fn rollback_restores_the_staged_inode_without_clobbering() {
        let root = crate::test_tempdir().expect("rollback root");
        let source = root.path().join("movie.mkv");
        tokio::fs::write(&source, b"original profile seven")
            .await
            .expect("source bytes");
        let file = media_file(source.clone());
        let paths = ConversionPaths::for_file(&file).expect("paths");
        prepare_fresh_directory(&paths, &file)
            .await
            .expect("scratch");
        assert!(rename_noreplace_durable(&source, &paths.staged_original)
            .await
            .expect("stage original"));
        tokio::fs::write(&source, b"invalid published replacement")
            .await
            .expect("bad publication");

        rollback_published(&file, &paths).await.expect("rollback");
        assert_eq!(
            tokio::fs::read(&source).await.expect("restored source"),
            b"original profile seven"
        );
        assert!(!path_entry_exists(&paths.staged_original)
            .await
            .expect("staged absence"));
    }

    #[tokio::test]
    async fn recovery_finalizes_a_staged_original_before_commit() {
        let root = crate::test_tempdir().expect("recovery root");
        let source = root.path().join("movie.mkv");
        tokio::fs::write(&source, b"original profile seven")
            .await
            .expect("source bytes");
        let file = media_file(source.clone());
        let paths = ConversionPaths::for_file(&file).expect("paths");
        prepare_fresh_directory(&paths, &file)
            .await
            .expect("scratch");
        assert!(rename_noreplace_durable(&source, &paths.staged_original)
            .await
            .expect("stage original"));
        tokio::fs::write(&source, b"profile eight")
            .await
            .expect("published bytes");

        let retained = finalize_staged_original(&paths, true)
            .await
            .expect("retention")
            .expect("retained path");
        assert_eq!(retained, paths.retained_original.to_str().expect("UTF-8"));
        assert_eq!(
            tokio::fs::read(&paths.retained_original)
                .await
                .expect("retained bytes"),
            b"original profile seven"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_refused_before_scratch_is_derived() {
        use std::os::unix::ffi::OsStringExt;

        let root = crate::test_tempdir().expect("path root");
        let valid = root.path().join("placeholder.mkv");
        std::fs::write(&valid, b"source").expect("placeholder");
        let mut file = media_file(valid);
        let mut path = root.path().to_path_buf();
        path.push(std::ffi::OsString::from_vec(vec![b'm', 0xff, b'v']));
        file.path = path;
        assert!(ConversionPaths::for_file(&file)
            .expect_err("non-UTF-8 refusal")
            .contains("not valid UTF-8"));
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
