//! Permanent Dolby Vision Profile 7 to 8.1 conversion.
//!
//! This is intentionally separate from the playback copy pipe. The output is
//! proved as a complete replacement before the source pathname moves, and the
//! scanner then sees an ordinary Profile 8 file on its next read.

#[cfg(unix)]
use std::ffi::CString;
use std::ffi::{OsStr, OsString};
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use plurx_core::domain::{MediaFile, ProbeResult};
use plurx_core::fs_secure::SecureDirectory;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
#[cfg(test)]
use tokio::io::AsyncSeekExt;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_OUTPUT: usize = 32 * 1024;
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_RUN_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_MEDIA_PROBE_OUTPUT: usize = 4 * 1024 * 1024;
const MEDIA_PROBE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SCRATCH_OWNER_FILE: &str = ".plurx-dv-owner.json";
const SCRATCH_OWNER_VERSION: u32 = 5;
const MAX_SCRATCH_OWNER_BYTES: u64 = 32 * 1024;
const MAX_SCRATCH_ENTRIES: usize = 16;
const HASH_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const DISCARD_PENDING_FILE: &str = "source.p7.discard-pending";
const PUBLIC_PROOF_FILE: &str = "replacement.p81.public-proof";
// Destructive namespace retirement is allowed only after the candidate has
// entered this daemon-owned capability. Processes running as the plurxd uid
// are trusted; other users cannot replace children below a 0700 directory.
const PRIVATE_DELETE_ANCHOR: &str = ".plurx-dv-delete-anchor-v1";
const CLEANUP_STATE_VERSION: u32 = 1;
const MAX_CLEANUP_STATE_BYTES: u64 = 4 * 1024;
const RECOVERY_GUARD_WITNESS_VERSION: u32 = 1;
const MAX_RECOVERY_GUARD_WITNESS_BYTES: u64 = 16 * 1024;
#[cfg(unix)]
const CHILD_MEDIA_FD_MIN: i32 = 198;

struct HashWorkerState {
    semaphore: Arc<tokio::sync::Semaphore>,
    residual_io: AtomicBool,
}

impl HashWorkerState {
    fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            residual_io: AtomicBool::new(false),
        }
    }
}

static HASH_WORKER: OnceLock<Arc<HashWorkerState>> = OnceLock::new();

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
    let mut command = tokio::process::Command::new(command);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("Dolby Vision tool probe"),
    )?;
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
    read_bounded_stream(reader, MAX_TOOL_OUTPUT, "tool version output").await
}

async fn read_bounded_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    limit: usize,
    role: &str,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(io::Error::other(format!("{role} exceeded {limit} bytes")));
    }
    Ok(bytes)
}

fn mkvmerge_major(version: &str) -> Option<u64> {
    let marker = version.find("mkvmerge v")? + "mkvmerge v".len();
    version[marker..].split('.').next()?.parse().ok()
}

fn dovi_tool_version(version: &str) -> Option<(u64, u64, u64)> {
    let banner = version.trim().strip_prefix("dovi_tool ")?;
    if banner.is_empty() || banner.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    let token = banner.strip_prefix('v').unwrap_or(banner);
    let mut parts = token.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    let patch = parts.next()?;
    if parts.next().is_some()
        || [major, minor, patch]
            .into_iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some((
        major.parse().ok()?,
        minor.parse().ok()?,
        patch.parse().ok()?,
    ))
}

#[derive(Clone, Debug)]
pub struct VerifiedReplacement {
    pub el_type: Option<&'static str>,
    pub bytes_after: i64,
}

#[derive(Debug)]
pub enum PublicationOutcome {
    Published(Box<PublishedReplacement>),
    SafelyRolledBack { reason: String },
}

#[derive(Debug)]
pub struct PublishedReplacement {
    pub original_path: Option<String>,
    pub recovery_guard_id: Option<String>,
    pub recovery_guard_path: Option<String>,
    pub bytes_after: i64,
    pub probe: ProbeResult,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Clone, Debug)]
pub struct RecoveryGuardIntent {
    pub guard_id: String,
    pub recovery_path: String,
}

#[derive(Clone, Debug)]
pub struct ConversionPaths {
    pub directory: PathBuf,
    directory_name: String,
    cleanup_name: String,
    cleanup_state_name: String,
    pub raw: PathBuf,
    pub converted: PathBuf,
    pub rpu: PathBuf,
    pub replacement: PathBuf,
    pub staged_original: PathBuf,
    pub retained_original: PathBuf,
}

impl ConversionPaths {
    pub fn for_file(file: &MediaFile) -> Result<Self, String> {
        Self::for_source(file.id, &file.path)
    }

    fn for_source(file_id: i64, source: &Path) -> Result<Self, String> {
        let path = source
            .to_str()
            .ok_or_else(|| "source path is not valid UTF-8".to_owned())?;
        let parent = source
            .parent()
            .ok_or_else(|| "source has no parent directory".to_owned())?;
        let name = source
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| "source filename is not valid UTF-8".to_owned())?;
        let directory_name = format!(".{name}.plurx-dv-{file_id}");
        let cleanup_name = format!("{directory_name}.cleanup");
        let cleanup_state_name = format!("{directory_name}.cleanup-state");
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
            cleanup_name,
            cleanup_state_name,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CleanupQuarantineState {
    version: u32,
    file_id: i64,
    quarantine_name: String,
    expected_device: u64,
    expected_inode: u64,
}

impl CleanupQuarantineState {
    fn new(
        file: &MediaFile,
        quarantine_name: String,
        root: plurx_core::fs_secure::FileIdentity,
    ) -> Self {
        Self {
            version: CLEANUP_STATE_VERSION,
            file_id: file.id,
            quarantine_name,
            expected_device: root.device,
            expected_inode: root.inode,
        }
    }

    fn owns_root(&self, root: plurx_core::fs_secure::FileIdentity) -> bool {
        self.expected_device == root.device && self.expected_inode == root.inode
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct LocalMediaIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl LocalMediaIdentity {
    fn same_inode_and_content_facts(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
    }

    fn object_version(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            self.device,
            self.inode,
            self.size,
            self.modified_seconds,
            self.modified_nanoseconds,
            self.changed_seconds,
            self.changed_nanoseconds
        )
    }
}

/// A mount-independent identity persisted in crash-recovery state. Device and
/// inode values are useful only while one node holds the mount that produced
/// them; a successor must instead prove the complete byte stream before it is
/// allowed to mutate any recovered artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct MediaContentIdentity {
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct NodeLocalBinding {
    node_id: String,
    identity: LocalMediaIdentity,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct PersistedMediaIdentity {
    content: MediaContentIdentity,
    local: NodeLocalBinding,
}

impl PersistedMediaIdentity {
    fn rebaseline(&mut self, node_id: &str, identity: LocalMediaIdentity) {
        self.local = NodeLocalBinding {
            node_id: node_id.to_owned(),
            identity,
        };
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ScratchOwner {
    version: u32,
    file_id: i64,
    source_path: String,
    source: PersistedMediaIdentity,
    replacement: Option<PersistedMediaIdentity>,
    expected_bytes: Option<i64>,
    keep_original: Option<bool>,
    rollback_reason: Option<String>,
    rollback_restore: Option<PersistedMediaIdentity>,
    discard_pending: bool,
    recovery_guard_id: Option<String>,
}

impl ScratchOwner {
    fn new(file: &MediaFile, source: PersistedMediaIdentity) -> Result<Self, String> {
        Ok(Self {
            version: SCRATCH_OWNER_VERSION,
            file_id: file.id,
            source_path: file
                .path
                .to_str()
                .ok_or_else(|| "source path is not valid UTF-8".to_owned())?
                .to_owned(),
            source,
            replacement: None,
            expected_bytes: None,
            keep_original: None,
            rollback_reason: None,
            rollback_restore: None,
            discard_pending: false,
            recovery_guard_id: None,
        })
    }

    fn owns_file(&self, file: &MediaFile) -> bool {
        self.version == SCRATCH_OWNER_VERSION
            && self.file_id == file.id
            && file.path.to_str() == Some(self.source_path.as_str())
    }
}

struct OwnedScratch {
    directory: SecureDirectory,
    owner: ScratchOwner,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryGuardWitnessState {
    GuardAttested,
    ScratchRemoved,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RecoveryGuardWitness {
    version: u32,
    guard_id: String,
    file_id: i64,
    source_path: String,
    state: RecoveryGuardWitnessState,
}

/// Keeps the exact source-parent filesystem capability alive until the fenced
/// Store deletion finishes. The terminal witness is intentionally left on the
/// media filesystem as a tiny tombstone after this token is dropped.
pub(crate) struct RecoveryGuardTombstoneAttestation {
    _source_parent: SecureDirectory,
}

pub async fn build_and_verify(
    tools: &DvDiskTools,
    file: &MediaFile,
    node_id: &str,
    loss: &CancellationToken,
    keep_original: bool,
) -> Result<VerifiedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    prepare_fresh_directory(&paths, file, node_id, loss).await?;

    // Bind the source once before any child can consume it. Both source-facing
    // tools inherit a duplicate of this descriptor; a reusable pathname is
    // never handed to either process.
    let mut scratch = require_owned_scratch(&paths, file, None).await?;
    let source_parent = open_parent(&file.path, "media source").await?;
    let source_name = path_child_name(&file.path, "media source")?;
    let (tool_source, source_rebound) = bind_expected_content(
        &source_parent,
        &source_name,
        &scratch.owner.source,
        node_id,
        loss,
        "source before tool pipeline",
    )
    .await?;
    if source_rebound {
        scratch
            .owner
            .source
            .rebaseline(node_id, tool_source.local.clone());
        write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    }

    run_tool_with_bound_media(
        &tools.ffmpeg,
        &[
            "-nostdin".into(),
            "-v".into(),
            "error".into(),
            "-i".into(),
            OsString::new(),
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
        4,
        &tool_source,
        "source during ffmpeg extraction",
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

    run_tool_with_bound_media(
        &tools.mkvmerge,
        &[
            "-o".into(),
            paths.replacement.as_os_str().to_owned(),
            paths.converted.as_os_str().to_owned(),
            "--no-video".into(),
            OsString::new(),
        ],
        4,
        &tool_source,
        "source during mkvmerge remux",
        loss,
    )
    .await?;
    require_nonempty(&paths.replacement).await?;

    let (source, source_rebound) = bind_expected_content(
        &source_parent,
        &source_name,
        &scratch.owner.source,
        node_id,
        loss,
        "source before commit",
    )
    .await?;
    if source_rebound {
        scratch
            .owner
            .source
            .rebaseline(node_id, source.local.clone());
        write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    }
    let replacement = bind_child_fresh(
        &scratch.directory,
        "replacement.mkv",
        loss,
        "replacement before commit",
    )
    .await?;
    let source_probe = probe_bound(&source, &file.path, "source before commit", loss).await?;
    let replacement_probe = probe_bound(
        &replacement,
        &paths.replacement,
        "replacement before commit",
        loss,
    )
    .await?;
    verify_replacement(&source_probe, &replacement_probe)?;
    let bytes_after = verified_replacement_bytes(&replacement)?;
    replacement
        .file
        .sync_all()
        .await
        .map_err(|error| format!("syncing verified replacement: {error}"))?;
    require_held_unchanged(&replacement, "replacement after durable sync").await?;
    scratch.owner.replacement = Some(persisted_binding(&replacement, node_id));
    scratch.owner.expected_bytes = Some(bytes_after);
    // The operator's retention choice belongs to this verified attempt. A
    // later global setting change must not reinterpret crash-recovery state.
    scratch.owner.keep_original = Some(keep_original);
    scratch.owner.recovery_guard_id = (!keep_original).then(|| uuid::Uuid::new_v4().to_string());
    if loss.is_cancelled() {
        return Err("conversion lease was lost before recording verified artifacts".to_owned());
    }
    write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    Ok(VerifiedReplacement {
        el_type,
        bytes_after,
    })
}

fn verified_replacement_bytes(replacement: &BoundMedia) -> Result<i64, String> {
    if replacement.content.size == 0 {
        return Err("replacement is empty before verification".to_owned());
    }
    i64::try_from(replacement.content.size)
        .map_err(|_| "replacement is too large to record".to_owned())
}

pub async fn verify_existing(
    file: &MediaFile,
    node_id: &str,
    loss: &CancellationToken,
    el_type: Option<&'static str>,
    expected_bytes: i64,
) -> Result<VerifiedReplacement, String> {
    let paths = ConversionPaths::for_file(file)?;
    let mut scratch = require_owned_scratch(&paths, file, None).await?;
    if scratch.owner.expected_bytes != Some(expected_bytes) {
        return Err(
            "verified ledger byte count does not match the owned scratch manifest".to_owned(),
        );
    }
    let expected_replacement = scratch
        .owner
        .replacement
        .clone()
        .ok_or_else(|| "owned scratch has no verified replacement identity".to_owned())?;
    if expected_replacement.content.size != expected_bytes.max(0) as u64 {
        return Err("verified replacement identity has the wrong byte count".to_owned());
    }
    let source_exists = path_entry_exists(&file.path).await?;
    let staged_exists = child_exists(&scratch.directory, "source.p7.original").await?;
    let replacement_exists = child_exists(&scratch.directory, "replacement.mkv").await?;
    if staged_exists && source_exists && replacement_exists {
        return Err(
            "source, staged original, and replacement all exist; refusing ambiguous recovery"
                .to_owned(),
        );
    }
    if !(staged_exists && source_exists && !replacement_exists
        || source_exists && replacement_exists && !staged_exists
        || staged_exists && replacement_exists && !source_exists)
    {
        return Err("verified conversion artifacts cannot be recovered".to_owned());
    }
    let source_parent = open_parent(&file.path, "media source").await?;
    let source_name = path_child_name(&file.path, "media source")?;
    let expected_source = scratch.owner.source.clone();
    let (original, source_rebound) = if staged_exists {
        bind_expected_content(
            &scratch.directory,
            "source.p7.original",
            &expected_source,
            node_id,
            loss,
            "recovery original",
        )
        .await?
    } else {
        bind_expected_content(
            &source_parent,
            &source_name,
            &expected_source,
            node_id,
            loss,
            "recovery original",
        )
        .await?
    };
    if source_rebound {
        scratch
            .owner
            .source
            .rebaseline(node_id, original.local.clone());
        write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    }
    let (replacement, replacement_rebound) = if replacement_exists {
        bind_expected_content(
            &scratch.directory,
            "replacement.mkv",
            &expected_replacement,
            node_id,
            loss,
            "recovery replacement",
        )
        .await?
    } else {
        bind_expected_content(
            &source_parent,
            &source_name,
            &expected_replacement,
            node_id,
            loss,
            "recovery replacement",
        )
        .await?
    };
    if replacement_rebound {
        scratch
            .owner
            .replacement
            .as_mut()
            .expect("verified replacement exists")
            .rebaseline(node_id, replacement.local.clone());
        write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    }
    let source_probe = probe_bound(&original, &file.path, "original for recovery", loss).await?;
    let replacement_probe =
        probe_bound(&replacement, &file.path, "replacement for recovery", loss).await?;
    verify_replacement(&source_probe, &replacement_probe)?;
    let bytes_after = i64::try_from(replacement.content.size)
        .map_err(|_| "replacement is too large to record".to_owned())?;
    if bytes_after != expected_bytes {
        return Err(format!(
            "replacement is {bytes_after} bytes, verified ledger recorded {expected_bytes}"
        ));
    }
    Ok(VerifiedReplacement {
        el_type,
        bytes_after,
    })
}

async fn prepare_fresh_directory(
    paths: &ConversionPaths,
    file: &MediaFile,
    node_id: &str,
    loss: &CancellationToken,
) -> Result<(), String> {
    let source = current_source_binding(file, loss).await?;
    let source = persisted_binding(&source, node_id);
    reconcile_cleanup_quarantine(paths, file, loss).await?;
    if path_entry_exists(&paths.directory).await? {
        let scratch = require_owned_scratch(paths, file, Some(&source)).await?;
        match scratch.directory.child_metadata("source.p7.original").await {
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
        remove_owned_scratch(paths, file, Some(&source), loss).await?;
    }
    create_owned_scratch(paths, &ScratchOwner::new(file, source)?).await
}

struct BoundMedia {
    file: tokio::fs::File,
    local: LocalMediaIdentity,
    content: MediaContentIdentity,
}

#[cfg(test)]
thread_local! {
    static HASH_ROLES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
struct FinalizeSwapHook {
    device: u64,
    inode: u64,
    target: &'static str,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static FINALIZE_SWAP_HOOKS: std::sync::Mutex<Vec<FinalizeSwapHook>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
struct ProofCreationSwapHook {
    device: u64,
    inode: u64,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static PROOF_CREATION_SWAP_HOOK: std::sync::Mutex<Option<ProofCreationSwapHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
struct PublicRestoreSwapHook {
    device: u64,
    inode: u64,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static PUBLIC_RESTORE_SWAP_HOOK: std::sync::Mutex<Option<PublicRestoreSwapHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
struct CleanupIntentHook {
    device: u64,
    inode: u64,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static CLEANUP_INTENT_HOOK: std::sync::Mutex<Option<CleanupIntentHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
struct NamespaceRemovalSwapHook {
    device: u64,
    inode: u64,
    target: String,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static FINAL_UNLINK_SWAP_HOOKS: std::sync::Mutex<Vec<NamespaceRemovalSwapHook>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
static FINAL_RMDIR_SWAP_HOOKS: std::sync::Mutex<Vec<NamespaceRemovalSwapHook>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
struct RenameReopenFailureHook {
    from_device: u64,
    from_inode: u64,
    to_device: u64,
    to_inode: u64,
    from_name: String,
    to_name: String,
    action: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
static RENAME_REOPEN_FAILURE_HOOKS: std::sync::Mutex<Vec<RenameReopenFailureHook>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
#[allow(clippy::unnecessary_cast)]
fn stat_device_id(stat: &libc::stat) -> u64 {
    // `dev_t` is already `u64` on Linux but has a different width on macOS.
    stat.st_dev as u64
}

#[cfg(test)]
fn reset_hash_passes() {
    HASH_ROLES.with(|roles| roles.borrow_mut().clear());
}

#[cfg(test)]
fn hash_roles() -> Vec<String> {
    HASH_ROLES.with(|roles| roles.borrow().clone())
}

async fn current_source_binding(
    file: &MediaFile,
    loss: &CancellationToken,
) -> Result<BoundMedia, String> {
    let parent = open_parent(&file.path, "media source").await?;
    let name = path_child_name(&file.path, "media source")?;
    let bound = bind_child_fresh(&parent, &name, loss, "media source").await?;
    let metadata = bound
        .file
        .metadata()
        .await
        .map_err(|error| format!("fstat {}: {error}", file.path.display()))?;
    if !metadata.is_file() {
        return Err("source is not a regular file".to_owned());
    }
    if metadata.len() != file.size.max(0) as u64 || modified_seconds(&metadata) != file.mtime {
        return Err("source no longer matches the scanner's size/mtime identity".to_owned());
    }
    Ok(bound)
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> Result<LocalMediaIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(LocalMediaIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn metadata_identity(metadata: &std::fs::Metadata) -> Result<LocalMediaIdentity, String> {
    let modified = metadata
        .modified()
        .map_err(|error| format!("reading source modification time: {error}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("source modification time precedes unix epoch: {error}"))?;
    Ok(LocalMediaIdentity {
        device: 0,
        inode: 0,
        size: metadata.len(),
        modified_seconds: modified.as_secs().min(i64::MAX as u64) as i64,
        modified_nanoseconds: modified.subsec_nanos() as i64,
        changed_seconds: modified.as_secs().min(i64::MAX as u64) as i64,
        changed_nanoseconds: modified.subsec_nanos() as i64,
    })
}

#[cfg(unix)]
async fn held_media_identity(
    _file: &tokio::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<LocalMediaIdentity, String> {
    metadata_identity(metadata)
}

#[cfg(windows)]
async fn held_media_identity(
    file: &tokio::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<LocalMediaIdentity, String> {
    plurx_core::fs_secure::async_file_identity(file)
        .map(local_identity_from_secure)
        .map_err(|error| format!("reading Windows file identity: {error}"))
}

#[cfg(test)]
async fn hash_held_exact<R>(
    file: &mut R,
    expected_size: u64,
    loss: &CancellationToken,
    deadline: tokio::time::Instant,
    role: &str,
) -> Result<MediaContentIdentity, String>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    #[cfg(test)]
    HASH_ROLES.with(|roles| roles.borrow_mut().push(role.to_owned()));
    let hash = hash_held_exact_inner(file, expected_size, role);
    tokio::pin!(hash);
    tokio::select! {
        biased;
        () = loss.cancelled() => {
            Err(format!("hashing {role} cancelled after conversion lease loss"))
        }
        result = tokio::time::timeout_at(deadline, &mut hash) => {
            result.map_err(|_| format!("hashing {role} exceeded its deadline"))?
        }
    }
}

#[cfg(test)]
async fn hash_held_exact_inner<R>(
    file: &mut R,
    expected_size: u64,
    role: &str,
) -> Result<MediaContentIdentity, String>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| format!("seeking {role} before hashing: {error}"))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    while size < expected_size {
        let remaining = expected_size.saturating_sub(size);
        let ceiling = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| format!("{role} hash chunk is too large"))?;
        let read = file
            .read(&mut buffer[..ceiling])
            .await
            .map_err(|error| format!("hashing {role}: {error}"))?;
        if read == 0 {
            return Err(format!(
                "{role} ended after {size} bytes, expected {expected_size}"
            ));
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| format!("{role} is too large to hash"))?;
        hasher.update(&buffer[..read]);
    }
    let mut extra = [0u8; 1];
    let extra = file
        .read(&mut extra)
        .await
        .map_err(|error| format!("checking {role} hash EOF: {error}"))?;
    if extra != 0 {
        return Err(format!(
            "{role} grew beyond its expected {expected_size} bytes"
        ));
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| format!("rewinding {role} after hashing: {error}"))?;
    Ok(MediaContentIdentity {
        size,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

async fn hash_regular_file_exact(
    file: &tokio::fs::File,
    expected_size: u64,
    loss: &CancellationToken,
    deadline: tokio::time::Instant,
    role: &str,
) -> Result<MediaContentIdentity, String> {
    #[cfg(test)]
    HASH_ROLES.with(|roles| roles.borrow_mut().push(role.to_owned()));
    let state = HASH_WORKER
        .get_or_init(|| Arc::new(HashWorkerState::new()))
        .clone();
    #[cfg(unix)]
    let owned = duplicate_hash_file(file, role)?;
    #[cfg(windows)]
    let owned = duplicate_hash_file(file, role).await?;
    let loss_for_worker = loss.clone();
    let role_owned = role.to_owned();
    let wall_deadline = deadline.into_std();
    run_bounded_hash_job(state, loss, deadline, role, move || {
        hash_regular_file_exact_blocking(
            owned,
            expected_size,
            &loss_for_worker,
            wall_deadline,
            &role_owned,
        )
    })
    .await
}

#[cfg(unix)]
fn duplicate_hash_file(file: &tokio::fs::File, role: &str) -> Result<std::fs::File, String> {
    let duplicated = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(format!(
            "duplicating held descriptor for hashing {role}: {}",
            io::Error::last_os_error()
        ));
    }
    // Own the duplicated descriptor before the blocking job is scheduled. A
    // timeout may return while a filesystem syscall remains blocked, so a raw
    // descriptor borrowed from `file` must never outlive this async frame.
    Ok(std::fs::File::from(unsafe {
        OwnedFd::from_raw_fd(duplicated)
    }))
}

#[cfg(windows)]
async fn duplicate_hash_file(file: &tokio::fs::File, role: &str) -> Result<std::fs::File, String> {
    let cloned = file
        .try_clone()
        .await
        .map_err(|error| format!("duplicating held handle for hashing {role}: {error}"))?;
    Ok(cloned.into_std().await)
}

async fn run_bounded_hash_job<F>(
    state: Arc<HashWorkerState>,
    loss: &CancellationToken,
    deadline: tokio::time::Instant,
    role: &str,
    job: F,
) -> Result<MediaContentIdentity, String>
where
    F: FnOnce() -> Result<MediaContentIdentity, String> + Send + 'static,
{
    if state.residual_io.load(Ordering::SeqCst) {
        return Err(format!(
            "hashing {role} refused because an earlier timed-out filesystem read is still occupying the bounded hash worker"
        ));
    }
    let semaphore = Arc::clone(&state.semaphore);
    let permit = tokio::select! {
        biased;
        () = loss.cancelled() => {
            return Err(format!("hashing {role} cancelled while waiting for the bounded hash worker"));
        }
        result = tokio::time::timeout_at(deadline, semaphore.acquire_owned()) => {
            result
                .map_err(|_| format!("hashing {role} exceeded its deadline while waiting for the bounded hash worker"))?
                .map_err(|_| "bounded hash worker was closed".to_owned())?
        }
    };
    if state.residual_io.load(Ordering::SeqCst) {
        drop(permit);
        return Err(format!(
            "hashing {role} refused because an earlier timed-out filesystem read is still occupying the bounded hash worker"
        ));
    }
    let timed_out = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let timed_out_worker = Arc::clone(&timed_out);
    let completed_worker = Arc::clone(&completed);
    let state_worker = Arc::clone(&state);
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = job();
        completed_worker.store(true, Ordering::SeqCst);
        if timed_out_worker.load(Ordering::SeqCst) {
            state_worker.residual_io.store(false, Ordering::SeqCst);
        }
        result
    });
    tokio::pin!(worker);
    tokio::select! {
        biased;
        () = loss.cancelled() => {
            timed_out.store(true, Ordering::SeqCst);
            state.residual_io.store(true, Ordering::SeqCst);
            if completed.load(Ordering::SeqCst) {
                state.residual_io.store(false, Ordering::SeqCst);
            }
            Err(format!("hashing {role} cancelled after conversion lease loss; any blocked filesystem read remains charged to the bounded hash worker"))
        }
        result = tokio::time::timeout_at(deadline, &mut worker) => {
            match result {
                Ok(joined) => joined
                    .map_err(|error| format!("joining bounded hash worker for {role}: {error}"))?,
                Err(_) => {
                    timed_out.store(true, Ordering::SeqCst);
                    state.residual_io.store(true, Ordering::SeqCst);
                    if completed.load(Ordering::SeqCst) {
                        state.residual_io.store(false, Ordering::SeqCst);
                    }
                    Err(format!("hashing {role} exceeded its deadline; any blocked filesystem read remains charged to the bounded hash worker"))
                }
            }
        }
    }
}

fn hash_regular_file_exact_blocking(
    mut file: std::fs::File,
    expected_size: u64,
    loss: &CancellationToken,
    deadline: std::time::Instant,
    role: &str,
) -> Result<MediaContentIdentity, String> {
    use std::io::{Read, Seek};

    let check = || {
        if loss.is_cancelled() {
            Err(format!(
                "hashing {role} cancelled after conversion lease loss"
            ))
        } else if std::time::Instant::now() >= deadline {
            Err(format!("hashing {role} exceeded its deadline"))
        } else {
            Ok(())
        }
    };
    check()?;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|error| format!("seeking {role} before hashing: {error}"))?;
    check()?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    while size < expected_size {
        check()?;
        let remaining = expected_size.saturating_sub(size);
        let ceiling = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| format!("{role} hash chunk is too large"))?;
        let read = file
            .read(&mut buffer[..ceiling])
            .map_err(|error| format!("hashing {role}: {error}"))?;
        check()?;
        if read == 0 {
            return Err(format!(
                "{role} ended after {size} bytes, expected {expected_size}"
            ));
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| format!("{role} is too large to hash"))?;
        hasher.update(&buffer[..read]);
    }
    check()?;
    let mut extra = [0u8; 1];
    let extra = file
        .read(&mut extra)
        .map_err(|error| format!("checking {role} hash EOF: {error}"))?;
    check()?;
    if extra != 0 {
        return Err(format!(
            "{role} grew beyond its expected {expected_size} bytes"
        ));
    }
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|error| format!("rewinding {role} after hashing: {error}"))?;
    check()?;
    Ok(MediaContentIdentity {
        size,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

async fn bind_child_fresh(
    directory: &SecureDirectory,
    name: &str,
    loss: &CancellationToken,
    role: &str,
) -> Result<BoundMedia, String> {
    let file = directory
        .open_read_child(name)
        .await
        .map_err(|error| format!("opening {role}: {error}"))?;
    let before = file
        .metadata()
        .await
        .map_err(|error| format!("fstat {role} before hashing: {error}"))?;
    if !before.is_file() {
        return Err(format!("{role} is not a regular file"));
    }
    let local = held_media_identity(&file, &before).await?;
    let content = hash_regular_file_exact(
        &file,
        local.size,
        loss,
        tokio::time::Instant::now() + HASH_TIMEOUT,
        role,
    )
    .await?;
    let after = file
        .metadata()
        .await
        .map_err(|error| format!("fstat {role} after hashing: {error}"))?;
    if local != held_media_identity(&file, &after).await? || content.size != after.len() {
        return Err(format!("{role} changed while its bytes were bound"));
    }
    Ok(BoundMedia {
        file,
        local,
        content,
    })
}

fn persisted_binding(media: &BoundMedia, node_id: &str) -> PersistedMediaIdentity {
    PersistedMediaIdentity {
        content: media.content.clone(),
        local: NodeLocalBinding {
            node_id: node_id.to_owned(),
            identity: media.local.clone(),
        },
    }
}

async fn bind_expected_content(
    directory: &SecureDirectory,
    name: &str,
    expected: &PersistedMediaIdentity,
    node_id: &str,
    loss: &CancellationToken,
    role: &str,
) -> Result<(BoundMedia, bool), String> {
    let file = directory
        .open_read_child(name)
        .await
        .map_err(|error| format!("opening {role}: {error}"))?;
    let before = file
        .metadata()
        .await
        .map_err(|error| format!("fstat {role}: {error}"))?;
    if !before.is_file() {
        return Err(format!("{role} is not a regular file"));
    }
    let local = held_media_identity(&file, &before).await?;
    if expected.local.node_id == node_id && expected.local.identity == local {
        return Ok((
            BoundMedia {
                file,
                local,
                content: expected.content.clone(),
            },
            false,
        ));
    }
    let content = hash_regular_file_exact(
        &file,
        expected.content.size,
        loss,
        tokio::time::Instant::now() + HASH_TIMEOUT,
        role,
    )
    .await?;
    let after = file
        .metadata()
        .await
        .map_err(|error| format!("re-fstat {role}: {error}"))?;
    if local != held_media_identity(&file, &after).await? || content != expected.content {
        return Err(format!("{role} does not match the manifest-bound bytes"));
    }
    Ok((
        BoundMedia {
            file,
            local,
            content,
        },
        true,
    ))
}

async fn bind_one_of(
    directory: &SecureDirectory,
    name: &str,
    first: &PersistedMediaIdentity,
    second: &PersistedMediaIdentity,
    node_id: &str,
    loss: &CancellationToken,
    role: &str,
) -> Result<(BoundMedia, bool, bool), String> {
    let file = directory
        .open_read_child(name)
        .await
        .map_err(|error| format!("opening {role}: {error}"))?;
    let before = file
        .metadata()
        .await
        .map_err(|error| format!("fstat {role}: {error}"))?;
    if !before.is_file() {
        return Err(format!("{role} is not a regular file"));
    }
    let local = held_media_identity(&file, &before).await?;
    if first.local.node_id == node_id && first.local.identity == local {
        return Ok((
            BoundMedia {
                file,
                local,
                content: first.content.clone(),
            },
            true,
            false,
        ));
    }
    if second.local.node_id == node_id && second.local.identity == local {
        return Ok((
            BoundMedia {
                file,
                local,
                content: second.content.clone(),
            },
            false,
            false,
        ));
    }
    if local.size != first.content.size && local.size != second.content.size {
        return Err(format!(
            "{role} size {} does not match either manifest candidate",
            local.size
        ));
    }
    let content = hash_regular_file_exact(
        &file,
        local.size,
        loss,
        tokio::time::Instant::now() + HASH_TIMEOUT,
        role,
    )
    .await?;
    let after = file
        .metadata()
        .await
        .map_err(|error| format!("re-fstat {role}: {error}"))?;
    if local != held_media_identity(&file, &after).await? {
        return Err(format!("{role} changed while its bytes were rebound"));
    }
    let is_first = if content == first.content {
        true
    } else if content == second.content {
        false
    } else {
        return Err(format!(
            "{role} does not match either manifest-bound object"
        ));
    };
    Ok((
        BoundMedia {
            file,
            local,
            content,
        },
        is_first,
        true,
    ))
}

async fn require_held_unchanged(media: &BoundMedia, role: &str) -> Result<(), String> {
    let current = media
        .file
        .metadata()
        .await
        .map_err(|error| format!("fstat {role}: {error}"))?;
    if media.local != held_media_identity(&media.file, &current).await? {
        return Err(format!("{role} inode facts changed"));
    }
    Ok(())
}

#[cfg(unix)]
async fn probe_bound(
    media: &BoundMedia,
    display_path: &Path,
    role: &str,
    loss: &CancellationToken,
) -> Result<ProbeResult, String> {
    probe_bound_with(
        &crate::ffmpeg::ffprobe_bin(),
        media,
        display_path,
        role,
        loss,
        MEDIA_PROBE_TIMEOUT,
    )
    .await
}

#[cfg(unix)]
async fn probe_bound_with(
    ffprobe: &str,
    media: &BoundMedia,
    display_path: &Path,
    role: &str,
    loss: &CancellationToken,
    timeout: Duration,
) -> Result<ProbeResult, String> {
    use std::os::unix::process::CommandExt;

    require_held_unchanged(media, role).await?;
    // Keep clear of stdio and the low descriptors Command may use for its
    // exec-error/reporting pipes.
    const CHILD_MEDIA_FD: i32 = 198;
    #[cfg(target_os = "linux")]
    const CHILD_MEDIA_PATH: &str = "/proc/self/fd/198";
    #[cfg(not(target_os = "linux"))]
    const CHILD_MEDIA_PATH: &str = "/dev/fd/198";
    let source_fd = media.file.as_raw_fd();
    let mut command = tokio::process::Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            CHILD_MEDIA_PATH,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: the closure calls only async-signal-safe fd operations between
    // fork and exec. dup2 either creates descriptor 198 without CLOEXEC or the
    // fcntl branch clears CLOEXEC when the held file already occupies it.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if source_fd == CHILD_MEDIA_FD {
                let flags = libc::fcntl(CHILD_MEDIA_FD, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(CHILD_MEDIA_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(io::Error::last_os_error());
                }
            } else if libc::dup2(source_fd, CHILD_MEDIA_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("Dolby Vision conversion source probe"),
    )
    .map_err(|error| format!("starting ffprobe for {role}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("ffprobe stdout was not piped for {role}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("ffprobe stderr was not piped for {role}"))?;
    let probe = async move {
        tokio::try_join!(
            read_bounded_stream(stdout, MAX_MEDIA_PROBE_OUTPUT, "ffprobe stdout"),
            read_bounded_stream(stderr, MAX_MEDIA_PROBE_OUTPUT, "ffprobe stderr"),
            child.wait(),
        )
    };
    let (stdout, stderr, status) = tokio::select! {
        biased;
        () = loss.cancelled() => {
            return Err(format!("ffprobe for {role} cancelled after conversion lease loss"));
        }
        result = tokio::time::timeout(timeout, probe) => {
            result
                .map_err(|_| format!("ffprobe for {role} exceeded its deadline"))?
                .map_err(|error| format!("reading bounded ffprobe result for {role}: {error}"))?
        }
    };
    if !status.success() {
        return Err(format!(
            "ffprobe failed for {role} with {}: {}",
            status
                .code()
                .map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
            bounded_text(&stderr).trim()
        ));
    }
    let json: serde_json::Value = serde_json::from_slice(&stdout)
        .map_err(|error| format!("parsing ffprobe JSON for {role}: {error}"))?;
    let mut probe = plurx_core::scan::probe::parse_probe_json(&json);
    probe.container = display_path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_lowercase);
    require_held_unchanged(media, role).await?;
    Ok(probe)
}

#[cfg(windows)]
async fn probe_bound(
    media: &BoundMedia,
    display_path: &Path,
    role: &str,
    loss: &CancellationToken,
) -> Result<ProbeResult, String> {
    require_held_unchanged(media, role).await?;
    let held = media
        .file
        .try_clone()
        .await
        .map_err(|error| format!("duplicating held Windows media for {role}: {error}"))?
        .into_std()
        .await;
    let path = plurx_core::fs_secure::std_file_path(&held)
        .map_err(|error| format!("resolving held Windows media for {role}: {error}"))?;
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
    command
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("Dolby Vision conversion source probe"),
    )
    .map_err(|error| format!("starting ffprobe for {role}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("ffprobe stdout was not piped for {role}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("ffprobe stderr was not piped for {role}"))?;
    let probe = async move {
        tokio::try_join!(
            read_bounded_stream(stdout, MAX_MEDIA_PROBE_OUTPUT, "ffprobe stdout"),
            read_bounded_stream(stderr, MAX_MEDIA_PROBE_OUTPUT, "ffprobe stderr"),
            child.wait(),
        )
    };
    let (stdout, stderr, status) = tokio::select! {
        biased;
        () = loss.cancelled() => return Err(format!("ffprobe for {role} cancelled after conversion lease loss")),
        result = tokio::time::timeout(MEDIA_PROBE_TIMEOUT, probe) => {
            result.map_err(|_| format!("ffprobe for {role} exceeded its deadline"))?
                .map_err(|error| format!("reading bounded ffprobe result for {role}: {error}"))?
        }
    };
    require_held_unchanged(media, role).await?;
    if !status.success() {
        return Err(format!(
            "ffprobe failed for {role}: {}",
            bounded_text(&stderr).trim()
        ));
    }
    let json: serde_json::Value = serde_json::from_slice(&stdout)
        .map_err(|error| format!("parsing ffprobe JSON for {role}: {error}"))?;
    let mut probe = plurx_core::scan::probe::parse_probe_json(&json);
    probe.container = display_path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_lowercase);
    Ok(probe)
}

#[cfg(not(any(unix, windows)))]
async fn probe_bound(
    _media: &BoundMedia,
    _display_path: &Path,
    role: &str,
    _loss: &CancellationToken,
) -> Result<ProbeResult, String> {
    Err(format!(
        "descriptor-bound ffprobe is unsupported for {role}"
    ))
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
    expected_source: Option<&PersistedMediaIdentity>,
) -> Result<OwnedScratch, String> {
    let scratch = read_scratch_owner(paths).await?;
    if !scratch.owner.owns_file(file)
        || expected_source.is_some_and(|expected| scratch.owner.source.content != expected.content)
    {
        return Err(format!(
            "refusing conversion scratch not owned by this exact source: {}",
            paths.directory.display()
        ));
    }
    Ok(scratch)
}

async fn read_scratch_owner(paths: &ConversionPaths) -> Result<OwnedScratch, String> {
    let directory = SecureDirectory::open(&paths.directory)
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
    Ok(OwnedScratch { directory, owner })
}

async fn require_recovery_guard_scratch(
    paths: &ConversionPaths,
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<OwnedScratch, String> {
    let scratch = read_scratch_owner(paths).await?;
    if scratch.owner.version != SCRATCH_OWNER_VERSION
        || scratch.owner.file_id != file_id
        || scratch.owner.source_path != source_path
        || scratch.owner.keep_original != Some(false)
        || scratch.owner.recovery_guard_id.as_deref() != Some(guard_id)
    {
        return Err(format!(
            "refusing scratch not owned by recovery guard {guard_id}: {}",
            paths.directory.display()
        ));
    }
    Ok(scratch)
}

async fn write_scratch_owner(
    directory: &SecureDirectory,
    owner: &ScratchOwner,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(owner)
        .map_err(|error| format!("encoding conversion scratch owner: {error}"))?;
    if bytes.len() as u64 > MAX_SCRATCH_OWNER_BYTES {
        return Err("conversion scratch owner exceeds its bounded format".to_owned());
    }
    directory
        .atomic_write_child(SCRATCH_OWNER_FILE, &bytes)
        .await
        .map_err(|error| format!("writing conversion scratch owner: {error}"))
}

async fn child_identity(
    directory: &SecureDirectory,
    name: &str,
) -> Result<LocalMediaIdentity, String> {
    let file = directory
        .open_read_child(name)
        .await
        .map_err(|error| format!("opening scratch child {name}: {error}"))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| format!("fstat scratch child {name}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("scratch child {name} is not a regular file"));
    }
    held_media_identity(&file, &metadata).await
}

async fn child_exists(directory: &SecureDirectory, name: &str) -> Result<bool, String> {
    match directory.child_metadata(name).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspecting scratch child {name}: {error}")),
    }
}

fn path_child_name(path: &Path, role: &str) -> Result<String, String> {
    path.file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{role} filename is not valid UTF-8: {}", path.display()))
}

async fn open_parent(path: &Path, role: &str) -> Result<SecureDirectory, String> {
    SecureDirectory::open(
        path.parent()
            .ok_or_else(|| format!("{role} has no parent directory"))?,
    )
    .await
    .map_err(|error| format!("opening {role} parent safely: {error}"))
}

async fn create_owned_scratch(paths: &ConversionPaths, owner: &ScratchOwner) -> Result<(), String> {
    let scratch_path = paths.directory.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        #[cfg(windows)]
        let builder = std::fs::DirBuilder::new();
        builder.create(scratch_path)
    })
    .await
    .map_err(|error| format!("joining conversion scratch creation: {error}"))?
    .map_err(|error| format!("creating {}: {error}", paths.directory.display()))?;
    let directory = SecureDirectory::open(&paths.directory)
        .await
        .map_err(|error| format!("opening new conversion scratch safely: {error}"))?;
    write_scratch_owner(&directory, owner).await?;
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
    expected_source: Option<&PersistedMediaIdentity>,
    loss: &CancellationToken,
) -> Result<(), String> {
    reconcile_cleanup_quarantine(paths, file, loss).await?;
    if !path_entry_exists(&paths.directory).await? {
        return Ok(());
    }
    let scratch = require_owned_scratch(paths, file, expected_source).await?;
    let identity = scratch
        .directory
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
    let quarantine = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let state = CleanupQuarantineState::new(file, quarantine.clone(), identity);
    if loss.is_cancelled() {
        return Err("conversion lease was lost before recording cleanup intent".to_owned());
    }
    create_cleanup_state(&parent, paths, &state).await?;
    #[cfg(test)]
    run_cleanup_intent_hook(&scratch.directory).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup quarantine rename".to_owned());
    }
    if !parent
        .rename_child_noreplace(&paths.directory_name, &quarantine)
        .await
        .map_err(|error| format!("quarantining conversion scratch: {error}"))?
    {
        return Err(format!(
            "private conversion scratch quarantine {quarantine} unexpectedly exists"
        ));
    }
    sync_directory(parent_path).await?;
    let quarantined = parent
        .open_child_directory(&quarantine)
        .await
        .map_err(|error| format!("opening quarantined conversion scratch: {error}"))?;
    let quarantined_identity = quarantined
        .identity()
        .await
        .map_err(|error| format!("identifying quarantined conversion scratch: {error}"))?;
    if !quarantined_identity.same_inode(identity) {
        let restored = parent
            .rename_child_noreplace(&quarantine, &paths.directory_name)
            .await
            .map_err(|error| format!("restoring raced cleanup root: {error}"))?;
        if restored {
            sync_directory(parent_path).await?;
            return Err(
                "conversion scratch changed while it was quarantined and was restored".to_owned(),
            );
        }
        return Err(format!(
            "conversion scratch changed while it was quarantined; preserving private recovery root {quarantine}"
        ));
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup tree removal".to_owned());
    }
    remove_flat_tree_expected(
        &parent,
        &quarantine,
        &quarantined,
        quarantined_identity,
        loss,
    )
    .await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup state removal".to_owned());
    }
    unlink_cleanup_state(&parent, paths).await
}

#[cfg(test)]
async fn run_cleanup_intent_hook(directory: &SecureDirectory) -> Result<(), String> {
    let identity = directory
        .identity()
        .await
        .map_err(|error| format!("identifying cleanup failpoint root: {error}"))?;
    let mut hook = CLEANUP_INTENT_HOOK
        .lock()
        .expect("cleanup intent hook mutex");
    if hook
        .as_ref()
        .is_some_and(|hook| hook.device == identity.device && hook.inode == identity.inode)
    {
        let hook = hook.take().expect("matched cleanup intent hook exists");
        (hook.action)();
    }
    Ok(())
}

async fn reconcile_cleanup_quarantine(
    paths: &ConversionPaths,
    file: &MediaFile,
    loss: &CancellationToken,
) -> Result<(), String> {
    let parent_path = paths
        .directory
        .parent()
        .ok_or_else(|| "conversion scratch has no parent directory".to_owned())?;
    let parent = SecureDirectory::open(parent_path)
        .await
        .map_err(|error| format!("opening cleanup quarantine parent: {error}"))?;
    reconcile_private_cleanup_quarantine(paths, file, parent_path, &parent, loss).await?;

    // Version 3 used one deterministic cleanup name without a sidecar. Keep
    // this bounded migration path so an upgrade can finish an older crash.
    let final_cleanup_name = format!("{}.removing", paths.cleanup_name);
    if child_exists(&parent, &final_cleanup_name).await? {
        if child_exists(&parent, &paths.cleanup_name).await? {
            return Err("cleanup quarantine and one-shot removal root both exist".to_owned());
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before cleanup-root recovery".to_owned());
        }
        if !parent
            .rename_child_noreplace(&final_cleanup_name, &paths.cleanup_name)
            .await
            .map_err(|error| format!("restoring interrupted cleanup-root removal: {error}"))?
        {
            return Err("cleanup quarantine reappeared during removal recovery".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    if !logical_directory_exists(&parent, &paths.cleanup_name).await? {
        return Ok(());
    }
    let quarantine_visible = child_exists(&parent, &paths.cleanup_name).await?;
    let quarantine_path = if quarantine_visible {
        parent_path.join(&paths.cleanup_name)
    } else {
        parent_path
            .join(PRIVATE_DELETE_ANCHOR)
            .join(private_delete_slot(
                &paths.cleanup_name,
                PrivateDeleteKind::Directory,
            ))
    };
    let mut quarantine_paths = paths.clone();
    quarantine_paths.directory = quarantine_path;
    quarantine_paths.directory_name = paths.cleanup_name.clone();
    if !quarantine_visible {
        let reopened = SecureDirectory::open(&quarantine_paths.directory)
            .await
            .map_err(|error| format!("opening anchored legacy cleanup root: {error}"))?;
        let actual = reopened
            .identity()
            .await
            .map_err(|error| format!("identifying anchored legacy cleanup root: {error}"))?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before anchored legacy cleanup".to_owned());
        }
        return remove_flat_tree_expected(&parent, &paths.cleanup_name, &reopened, actual, loss)
            .await;
    }
    let scratch = require_owned_scratch(&quarantine_paths, file, None).await?;
    if logical_child_exists(&scratch.directory, "source.p7.original").await?
        || logical_child_exists(&scratch.directory, DISCARD_PENDING_FILE).await?
    {
        return Err("refusing cleanup quarantine that still contains a staged original".to_owned());
    }
    let expected = scratch
        .directory
        .identity()
        .await
        .map_err(|error| format!("identifying cleanup quarantine: {error}"))?;
    let reopened = SecureDirectory::open(&quarantine_paths.directory)
        .await
        .map_err(|error| format!("reopening cleanup quarantine: {error}"))?;
    let actual = reopened
        .identity()
        .await
        .map_err(|error| format!("re-identifying cleanup quarantine: {error}"))?;
    if !expected.same_inode(actual) {
        return Err("cleanup quarantine changed before reconciliation".to_owned());
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before legacy cleanup reconciliation".to_owned());
    }
    remove_flat_tree_expected(&parent, &paths.cleanup_name, &reopened, actual, loss).await
}

async fn write_cleanup_state(
    parent: &SecureDirectory,
    paths: &ConversionPaths,
    state: &CleanupQuarantineState,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("encoding cleanup quarantine state: {error}"))?;
    if bytes.len() as u64 > MAX_CLEANUP_STATE_BYTES {
        return Err("cleanup quarantine state exceeds its bounded format".to_owned());
    }
    parent
        .atomic_write_child(&paths.cleanup_state_name, &bytes)
        .await
        .map_err(|error| format!("persisting cleanup quarantine state: {error}"))
}

#[cfg(unix)]
async fn create_cleanup_state(
    parent: &SecureDirectory,
    paths: &ConversionPaths,
    state: &CleanupQuarantineState,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("encoding cleanup quarantine state: {error}"))?;
    if bytes.len() as u64 > MAX_CLEANUP_STATE_BYTES {
        return Err("cleanup quarantine state exceeds its bounded format".to_owned());
    }
    let parent = parent.clone();
    let destination = paths.cleanup_state_name.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let destination = CString::new(destination)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cleanup state has NUL"))?;
        let temporary = CString::new(format!(
            ".cleanup-state-{}.part",
            uuid::Uuid::new_v4().simple()
        ))
        .expect("static cleanup state prefix");
        let raw = unsafe {
            libc::openat(
                parent.raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let result = (|| {
            std::io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
            if !rename_noreplace_raw(parent.raw_fd(), &temporary, parent.raw_fd(), &destination)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cleanup state already belongs to another worker",
                ));
            }
            if unsafe { libc::fsync(parent.raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            unsafe { libc::unlinkat(parent.raw_fd(), temporary.as_ptr(), 0) };
        }
        result
    })
    .await
    .map_err(|error| format!("joining exclusive cleanup state creation: {error}"))?
    .map_err(|error| format!("creating exclusive cleanup state: {error}"))
}

async fn unlink_cleanup_state(
    parent: &SecureDirectory,
    paths: &ConversionPaths,
) -> Result<(), String> {
    if !logical_child_exists(parent, &paths.cleanup_state_name).await? {
        return Ok(());
    }
    let expected = if child_exists(parent, &paths.cleanup_state_name).await? {
        child_identity(parent, &paths.cleanup_state_name).await?
    } else {
        let anchor = SecureDirectory::open(
            &paths
                .directory
                .parent()
                .ok_or_else(|| "cleanup state has no parent".to_owned())?
                .join(PRIVATE_DELETE_ANCHOR),
        )
        .await
        .map_err(|error| format!("opening cleanup-state private delete anchor: {error}"))?;
        child_identity(
            &anchor,
            &private_delete_slot(&paths.cleanup_state_name, PrivateDeleteKind::File),
        )
        .await?
    };
    unlink_expected_child(parent, &paths.cleanup_state_name, &expected).await
}

async fn reconcile_private_cleanup_quarantine(
    paths: &ConversionPaths,
    file: &MediaFile,
    parent_path: &Path,
    parent: &SecureDirectory,
    loss: &CancellationToken,
) -> Result<(), String> {
    let delete_state_name = format!(".plurx-delete-{}", paths.cleanup_state_name);
    let final_delete_state_name = final_delete_quarantine_name(&paths.cleanup_state_name);
    if child_exists(parent, &final_delete_state_name).await? {
        if child_exists(parent, &delete_state_name).await? {
            return Err(
                "cleanup-state delete quarantine and one-shot removal child both exist".to_owned(),
            );
        }
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before cleanup-state removal recovery".to_owned(),
            );
        }
        if !parent
            .rename_child_noreplace(&final_delete_state_name, &delete_state_name)
            .await
            .map_err(|error| format!("restoring one-shot cleanup-state removal: {error}"))?
        {
            return Err("cleanup-state delete quarantine reappeared during recovery".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    if child_exists(parent, &delete_state_name).await? {
        if loss.is_cancelled() {
            return Err("conversion lease was lost before cleanup state recovery".to_owned());
        }
        if child_exists(parent, &paths.cleanup_state_name).await? {
            return Err(
                "cleanup state and its interrupted delete quarantine both exist".to_owned(),
            );
        }
        if !parent
            .rename_child_noreplace(&delete_state_name, &paths.cleanup_state_name)
            .await
            .map_err(|error| format!("restoring interrupted cleanup state deletion: {error}"))?
        {
            return Err("cleanup state reappeared during delete recovery".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    if !child_exists(parent, &paths.cleanup_state_name).await?
        && private_delete_pending(parent, &paths.cleanup_state_name, PrivateDeleteKind::File)
            .await?
    {
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before private cleanup-state recovery".to_owned(),
            );
        }
        restore_private_delete_slot(parent, &paths.cleanup_state_name, PrivateDeleteKind::File)
            .await?;
    }
    let bytes = match parent
        .read_bounded_child(&paths.cleanup_state_name, MAX_CLEANUP_STATE_BYTES)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("reading cleanup quarantine state: {error}")),
    };
    let mut state: CleanupQuarantineState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("refusing invalid cleanup quarantine state: {error}"))?;
    let prefix = format!("{}.", paths.cleanup_name);
    let suffix = state
        .quarantine_name
        .strip_prefix(&prefix)
        .ok_or_else(|| "cleanup quarantine state names an unsafe root".to_owned())?;
    if state.version != CLEANUP_STATE_VERSION
        || state.file_id != file.id
        || uuid::Uuid::parse_str(suffix).is_err()
    {
        return Err("cleanup quarantine state is not owned by this conversion".to_owned());
    }
    let final_quarantine_name = format!("{}.removing", state.quarantine_name);
    if child_exists(parent, &final_quarantine_name).await? {
        if child_exists(parent, &state.quarantine_name).await? {
            return Err("cleanup quarantine and one-shot removal root both exist".to_owned());
        }
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before private cleanup-root recovery".to_owned(),
            );
        }
        if !parent
            .rename_child_noreplace(&final_quarantine_name, &state.quarantine_name)
            .await
            .map_err(|error| format!("restoring one-shot private cleanup root: {error}"))?
        {
            return Err("private cleanup quarantine reappeared during recovery".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    let quarantine_exists = logical_directory_exists(parent, &state.quarantine_name).await?;
    if !quarantine_exists {
        // The durable intent may precede the rename, or the root removal may
        // precede intent cleanup. In either state, there is no private tree to
        // mutate and the identity-conditioned sidecar unlink converges.
        if loss.is_cancelled() {
            return Err("conversion lease was lost before cleanup state finalization".to_owned());
        }
        return unlink_cleanup_state(parent, paths).await;
    }

    let quarantine_visible = child_exists(parent, &state.quarantine_name).await?;
    let quarantine_path = if quarantine_visible {
        parent_path.join(&state.quarantine_name)
    } else {
        parent_path
            .join(PRIVATE_DELETE_ANCHOR)
            .join(private_delete_slot(
                &state.quarantine_name,
                PrivateDeleteKind::Directory,
            ))
    };
    let mut quarantine_paths = paths.clone();
    quarantine_paths.directory = quarantine_path;
    quarantine_paths.directory_name = state.quarantine_name.clone();
    let quarantine_directory = SecureDirectory::open(&quarantine_paths.directory)
        .await
        .map_err(|error| format!("opening private cleanup root for marker recovery: {error}"))?;
    if !quarantine_visible {
        let actual = quarantine_directory
            .identity()
            .await
            .map_err(|error| format!("identifying anchored private cleanup root: {error}"))?;
        if !state.owns_root(actual) {
            return Err(
                "anchored empty cleanup root no longer matches its durable sidecar".to_owned(),
            );
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before anchored private cleanup".to_owned());
        }
        remove_flat_tree_expected(
            parent,
            &state.quarantine_name,
            &quarantine_directory,
            actual,
            loss,
        )
        .await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before cleanup state removal".to_owned());
        }
        return unlink_cleanup_state(parent, paths).await;
    }
    let interrupted_owner = delete_quarantine_name(SCRATCH_OWNER_FILE);
    let final_interrupted_owner = final_delete_quarantine_name(SCRATCH_OWNER_FILE);
    if !child_exists(&quarantine_directory, SCRATCH_OWNER_FILE).await?
        && (child_exists(&quarantine_directory, &interrupted_owner).await?
            || child_exists(&quarantine_directory, &final_interrupted_owner).await?)
    {
        restore_interrupted_scratch_owner(&quarantine_paths, loss).await?;
    }
    drop(quarantine_directory);
    let reopened = SecureDirectory::open(&quarantine_paths.directory)
        .await
        .map_err(|error| format!("opening private cleanup quarantine: {error}"))?;
    let actual = reopened
        .identity()
        .await
        .map_err(|error| format!("identifying private cleanup quarantine: {error}"))?;
    if !state.owns_root(actual) {
        // A successor mount has different device/inode values. Rebind only
        // after the private directory proves the full owned scratch marker.
        require_owned_scratch(&quarantine_paths, file, None).await?;
        state.expected_device = actual.device;
        state.expected_inode = actual.inode;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before cleanup state rebaseline".to_owned());
        }
        write_cleanup_state(parent, paths, &state).await?;
    }
    if logical_child_exists(&reopened, "source.p7.original").await?
        || logical_child_exists(&reopened, DISCARD_PENDING_FILE).await?
    {
        return Err("refusing cleanup quarantine that still contains an original".to_owned());
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before private cleanup reconciliation".to_owned());
    }
    remove_flat_tree_expected(parent, &state.quarantine_name, &reopened, actual, loss).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup state removal".to_owned());
    }
    unlink_cleanup_state(parent, paths).await
}

#[cfg(unix)]
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

#[cfg(all(test, unix))]
async fn rename_noreplace_durable(from: &Path, to: &Path) -> Result<bool, String> {
    let from_parent = open_parent(from, "rename source").await?;
    let to_parent = open_parent(to, "rename destination").await?;
    let from_name = path_child_name(from, "rename source")?;
    let to_name = path_child_name(to, "rename destination")?;
    rename_noreplace_durable_between(&from_parent, &from_name, &to_parent, &to_name).await
}

#[cfg(all(test, unix))]
async fn rename_noreplace_durable_between(
    from_parent: &SecureDirectory,
    from_name: &str,
    to_parent: &SecureDirectory,
    to_name: &str,
) -> Result<bool, String> {
    let from_parent = from_parent.clone();
    let to_parent = to_parent.clone();
    let from_name = from_name.to_owned();
    let to_name = to_name.to_owned();
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
    .map_err(|error| format!("exclusive capability rename failed: {error}"))?;
    Ok(renamed)
}

#[cfg(unix)]
async fn rename_expected_noreplace_between(
    from_parent: &SecureDirectory,
    from_name: &str,
    expected: &LocalMediaIdentity,
    to_parent: &SecureDirectory,
    to_name: &str,
) -> Result<Option<LocalMediaIdentity>, String> {
    let from_parent = from_parent.clone();
    let to_parent = to_parent.clone();
    let from_name = from_name.to_owned();
    let to_name = to_name.to_owned();
    let expected = expected.clone();
    tokio::task::spawn_blocking(move || -> io::Result<Option<LocalMediaIdentity>> {
        #[cfg(test)]
        let hook_from_name = from_name.clone();
        #[cfg(test)]
        let hook_to_name = to_name.clone();
        let from = CString::new(from_name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename source has NUL"))?;
        let to = CString::new(to_name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "rename destination has NUL")
        })?;
        let raw = unsafe {
            libc::openat(
                from_parent.raw_fd(),
                from.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let held = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let before = metadata_identity(&held.metadata()?).map_err(io::Error::other)?;
        if before != expected {
            return Err(io::Error::other(
                "rename source no longer has the expected local identity",
            ));
        }
        #[cfg(target_os = "macos")]
        let renamed = unsafe {
            libc::renameatx_np(
                from_parent.raw_fd(),
                from.as_ptr(),
                to_parent.raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let renamed = unsafe {
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
        let renamed = -1;
        if renamed != 0 {
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive rename is unsupported on this platform",
            ));
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
        }
        sync_rename_parents(&from_parent, &to_parent).map_err(|error| {
            io::Error::other(format!(
                "rename reached its deterministic destination but parent sync failed: {error}"
            ))
        })?;
        #[cfg(test)]
        let force_reopen_failure = {
            let directory_identity = |fd| -> io::Result<(u64, u64)> {
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let stat = unsafe { stat.assume_init() };
                Ok((stat_device_id(&stat), stat.st_ino))
            };
            let (from_device, from_inode) = directory_identity(from_parent.raw_fd())?;
            let (to_device, to_inode) = directory_identity(to_parent.raw_fd())?;
            let mut hooks = RENAME_REOPEN_FAILURE_HOOKS
                .lock()
                .expect("rename reopen hook mutex");
            if let Some(position) = hooks.iter().position(|hook| {
                hook.from_device == from_device
                    && hook.from_inode == from_inode
                    && hook.to_device == to_device
                    && hook.to_inode == to_inode
                    && hook.from_name == hook_from_name
                    && hook.to_name == hook_to_name
            }) {
                let hook = hooks.remove(position);
                (hook.action)();
                true
            } else {
                false
            }
        };
        #[cfg(not(test))]
        let force_reopen_failure = false;
        let destination_raw = if force_reopen_failure {
            -1
        } else {
            unsafe {
                libc::openat(
                    to_parent.raw_fd(),
                    to.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            }
        };
        if destination_raw < 0 {
            let open_error = if force_reopen_failure {
                io::Error::other("injected post-rename reopen failure")
            } else {
                io::Error::last_os_error()
            };
            let restored = rename_noreplace_raw(
                to_parent.raw_fd(),
                &to,
                from_parent.raw_fd(),
                &from,
            );
            let sync_result = sync_rename_parents(&to_parent, &from_parent);
            return match (restored, sync_result) {
                (_, Err(sync_error)) => Err(io::Error::other(format!(
                    "renamed child could not be reopened and compensation parent sync failed: {open_error}; {sync_error}"
                ))),
                (Ok(true), Ok(())) => Err(io::Error::other(format!(
                    "renamed child could not be reopened and was restored: {open_error}"
                ))),
                (Ok(false), Ok(())) => Err(io::Error::other(format!(
                    "renamed child could not be reopened and remains at the synced deterministic destination because restoration was blocked: {open_error}"
                ))),
                (Err(restore_error), Ok(())) => Err(io::Error::other(format!(
                    "renamed child could not be reopened and remains at the synced deterministic destination because restoration failed: {open_error}; {restore_error}"
                ))),
            };
        }
        let destination = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(destination_raw) });
        let after = metadata_identity(&destination.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&after) {
            // The source name was swapped after it was inspected. Put that
            // unexpected winner back only if its original name is still free.
            let restored = rename_noreplace_raw(
                to_parent.raw_fd(),
                &to,
                from_parent.raw_fd(),
                &from,
            );
            sync_rename_parents(&to_parent, &from_parent)?;
            return match restored {
                Ok(true) => Err(io::Error::other(
                    "rename source was swapped during the capability operation and was restored",
                )),
                Ok(false) => Err(io::Error::other(
                    "rename source was swapped and remains at the synced deterministic destination because compensation was blocked",
                )),
                Err(error) => Err(io::Error::other(format!(
                    "rename source was swapped and remains at the synced deterministic destination because compensation failed: {error}"
                ))),
            };
        }
        Ok(Some(after))
    })
    .await
    .map_err(|error| format!("joining identity-conditioned rename: {error}"))?
    .map_err(|error| format!("identity-conditioned rename failed: {error}"))
}

#[cfg(unix)]
fn rename_noreplace_raw(
    from_parent: i32,
    from: &CString,
    to_parent: i32,
    to: &CString,
) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            from_parent,
            from.as_ptr(),
            to_parent,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_parent,
            from.as_ptr(),
            to_parent,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as i32
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = -1;
    if result == 0 {
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
}

#[cfg(unix)]
fn sync_rename_parents(first: &SecureDirectory, second: &SecureDirectory) -> io::Result<()> {
    sync_rename_parents_raw(first.raw_fd(), second.raw_fd())
}

#[cfg(unix)]
fn sync_rename_parents_raw(first: i32, second: i32) -> io::Result<()> {
    if unsafe { libc::fsync(first) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if first != second && unsafe { libc::fsync(second) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum PrivateDeleteKind {
    File,
    Directory,
}

fn private_delete_slot(name: &str, kind: PrivateDeleteKind) -> String {
    let mut hasher = Sha256::new();
    hasher.update(match kind {
        PrivateDeleteKind::File => b"file".as_slice(),
        PrivateDeleteKind::Directory => b"directory".as_slice(),
    });
    hasher.update((name.len() as u64).to_be_bytes());
    hasher.update(name.as_bytes());
    format!(
        "{}-{:x}",
        match kind {
            PrivateDeleteKind::File => "f",
            PrivateDeleteKind::Directory => "d",
        },
        hasher.finalize()
    )
}

#[cfg(unix)]
fn validate_private_delete_anchor(
    anchor: &std::fs::File,
    containing: &std::fs::Metadata,
) -> io::Result<()> {
    validate_private_delete_anchor_metadata(&anchor.metadata()?, containing, unsafe {
        libc::geteuid()
    })
}

#[cfg(unix)]
fn validate_private_delete_anchor_metadata(
    metadata: &std::fs::Metadata,
    containing: &std::fs::Metadata,
    daemon_uid: u32,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_dir()
        || metadata.uid() != daemon_uid
        || metadata.mode() & 0o7777 != 0o700
        || metadata.dev() != containing.dev()
    {
        return Err(io::Error::other(
            "private delete anchor must be a daemon-owned 0700 directory on the containing filesystem",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_private_delete_anchor_raw(
    containing_fd: i32,
    create: bool,
) -> io::Result<Option<std::fs::File>> {
    let containing_raw = unsafe { libc::dup(containing_fd) };
    if containing_raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let containing = unsafe { std::fs::File::from_raw_fd(containing_raw) };
    let anchor_name = CString::new(PRIVATE_DELETE_ANCHOR).expect("static private anchor name");
    let created = if create {
        if unsafe { libc::mkdirat(containing_fd, anchor_name.as_ptr(), 0o700) } == 0 {
            true
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            false
        }
    } else {
        false
    };
    let raw = unsafe {
        libc::openat(
            containing_fd,
            anchor_name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        let error = io::Error::last_os_error();
        if !create && error.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error);
    }
    let anchor = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    if created && unsafe { libc::fchmod(anchor.as_raw_fd(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    validate_private_delete_anchor(&anchor, &containing.metadata()?)?;
    if create && unsafe { libc::fsync(containing_fd) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(anchor))
}

#[cfg(unix)]
fn raw_child_exists(parent_fd: i32, name: &CString) -> io::Result<bool> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

#[cfg(unix)]
async fn private_delete_pending(
    directory: &SecureDirectory,
    name: &str,
    kind: PrivateDeleteKind,
) -> Result<bool, String> {
    let directory = directory.clone();
    let slot = private_delete_slot(name, kind);
    tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let Some(anchor) = open_private_delete_anchor_raw(directory.raw_fd(), false)? else {
            return Ok(false);
        };
        let slot = CString::new(slot).expect("hashed private slot has no NUL");
        raw_child_exists(anchor.as_raw_fd(), &slot)
    })
    .await
    .map_err(|error| format!("joining private delete inspection: {error}"))?
    .map_err(|error| format!("inspecting private delete anchor: {error}"))
}

#[cfg(unix)]
async fn restore_private_delete_slot(
    directory: &SecureDirectory,
    name: &str,
    kind: PrivateDeleteKind,
) -> Result<bool, String> {
    let directory = directory.clone();
    let name = name.to_owned();
    let slot = private_delete_slot(&name, kind);
    tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let Some(anchor) = open_private_delete_anchor_raw(directory.raw_fd(), false)? else {
            return Ok(false);
        };
        let slot = CString::new(slot).expect("hashed private slot has no NUL");
        if !raw_child_exists(anchor.as_raw_fd(), &slot)? {
            return Ok(false);
        }
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "restore name has NUL"))?;
        if !rename_noreplace_raw(anchor.as_raw_fd(), &slot, directory.raw_fd(), &name)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private delete recovery destination already exists",
            ));
        }
        sync_rename_parents_raw(anchor.as_raw_fd(), directory.raw_fd())?;
        Ok(true)
    })
    .await
    .map_err(|error| format!("joining private delete recovery: {error}"))?
    .map_err(|error| format!("restoring private delete candidate: {error}"))
}

async fn logical_child_exists(directory: &SecureDirectory, name: &str) -> Result<bool, String> {
    if child_exists(directory, name).await?
        || child_exists(directory, &delete_quarantine_name(name)).await?
        || child_exists(directory, &final_delete_quarantine_name(name)).await?
    {
        return Ok(true);
    }
    private_delete_pending(directory, name, PrivateDeleteKind::File).await
}

async fn logical_directory_exists(directory: &SecureDirectory, name: &str) -> Result<bool, String> {
    if child_exists(directory, name).await?
        || child_exists(directory, &format!("{name}.removing")).await?
    {
        return Ok(true);
    }
    private_delete_pending(directory, name, PrivateDeleteKind::Directory).await
}

#[cfg(unix)]
async fn drain_private_delete_anchor(
    directory: &SecureDirectory,
    loss: &CancellationToken,
) -> Result<(), String> {
    if !child_exists(directory, PRIVATE_DELETE_ANCHOR).await? {
        return Ok(());
    }
    let directory_for_validation = directory.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        open_private_delete_anchor_raw(directory_for_validation.raw_fd(), false)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "private anchor disappeared"))?;
        Ok(())
    })
    .await
    .map_err(|error| format!("joining private anchor validation: {error}"))?
    .map_err(|error| format!("validating private delete anchor: {error}"))?;
    let anchor = directory
        .open_child_directory(PRIVATE_DELETE_ANCHOR)
        .await
        .map_err(|error| format!("opening private delete anchor: {error}"))?;
    for name in anchor
        .child_names(MAX_SCRATCH_ENTRIES)
        .await
        .map_err(|error| format!("listing bounded private delete anchor: {error}"))?
    {
        if loss.is_cancelled() {
            return Err("conversion lease was lost during private delete recovery".to_owned());
        }
        let metadata = anchor
            .child_metadata(&name)
            .await
            .map_err(|error| format!("inspecting private delete slot {name}: {error}"))?;
        if !metadata.is_file {
            return Err(format!(
                "refusing nested or special private delete slot: {name}"
            ));
        }
        let owned_slot = [
            SCRATCH_OWNER_FILE,
            "BL_RPU.hevc",
            "BL_RPU.p81.hevc",
            "RPU.bin",
            "replacement.mkv",
            "source.p7.original",
            DISCARD_PENDING_FILE,
            PUBLIC_PROOF_FILE,
            FAILED_PUBLISHED_FILE,
        ]
        .into_iter()
        .any(|owned| private_delete_slot(owned, PrivateDeleteKind::File) == name);
        if !owned_slot {
            return Err(format!(
                "refusing unowned private delete slot during scratch cleanup: {name}"
            ));
        }
        let anchor = anchor.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let name = CString::new(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "private slot has NUL"))?;
            if unsafe { libc::unlinkat(anchor.raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::fsync(anchor.raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
        .await
        .map_err(|error| format!("joining private slot retirement: {error}"))?
        .map_err(|error| format!("retiring private delete slot: {error}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn remove_inner_private_anchor(directory: &std::fs::File) -> io::Result<()> {
    let Some(anchor) = open_private_delete_anchor_raw(directory.as_raw_fd(), false)? else {
        return Ok(());
    };
    drop(anchor);
    let name = CString::new(PRIVATE_DELETE_ANCHOR).expect("static private anchor name");
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
async fn unlink_expected_child(
    directory: &SecureDirectory,
    name: &str,
    expected: &LocalMediaIdentity,
) -> Result<(), String> {
    let directory = directory.clone();
    let name = name.to_owned();
    let expected = expected.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "delete name has NUL"))?;
        let quarantine = CString::new(delete_quarantine_name(&name.to_string_lossy()))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "quarantine name has NUL"))?;
        let legacy_final = CString::new(final_delete_quarantine_name(&name.to_string_lossy()))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final delete name has NUL"))?;
        let slot = CString::new(private_delete_slot(
            &name.to_string_lossy(),
            PrivateDeleteKind::File,
        ))
        .expect("hashed private slot has no NUL");
        let anchor = open_private_delete_anchor_raw(directory.raw_fd(), true)?
            .expect("create=true returns an anchor");

        // A crash after the exclusive move is resumed wholly below the held
        // private capability. The per-file lease serializes plurxd workers;
        // the 0700 uid boundary excludes pathname mutation by other users.
        if raw_child_exists(anchor.as_raw_fd(), &slot)? {
            let raw = unsafe {
                libc::openat(
                    anchor.as_raw_fd(),
                    slot.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let held = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let actual = metadata_identity(&held.metadata()?).map_err(io::Error::other)?;
            if !expected.same_inode_and_content_facts(&actual) {
                return Err(io::Error::other(
                    "private delete slot does not contain the expected candidate",
                ));
            }
            if unsafe { libc::unlinkat(anchor.as_raw_fd(), slot.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::fsync(anchor.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }

        let candidates = [&name, &quarantine, &legacy_final]
            .into_iter()
            .filter_map(|candidate| match raw_child_exists(directory.raw_fd(), candidate) {
                Ok(true) => Some(Ok(candidate)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<io::Result<Vec<_>>>()?;
        if candidates.len() != 1 {
            return Err(io::Error::other(
                "delete candidate has an ambiguous or absent recovery namespace",
            ));
        }
        let mut candidate = candidates[0];
        let raw = unsafe {
            libc::openat(
                directory.raw_fd(),
                candidate.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let held = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let actual = metadata_identity(&held.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&actual) {
            return Err(io::Error::other(
                "delete candidate no longer has the expected local identity",
            ));
        }
        if candidate.as_c_str() == name.as_c_str() {
            if !rename_noreplace_raw(
                directory.raw_fd(),
                &name,
                directory.raw_fd(),
                &quarantine,
            )? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "delete quarantine already exists",
                ));
            }
            if unsafe { libc::fsync(directory.raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            candidate = &quarantine;
        }
        let check_raw = unsafe {
            libc::openat(
                directory.raw_fd(),
                candidate.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if check_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let checked = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(check_raw) });
        let checked_identity = metadata_identity(&checked.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&checked_identity) {
            return Err(io::Error::other(
                "delete candidate changed before private-anchor entry",
            ));
        }
        #[cfg(test)]
        {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(directory.raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            let target = candidate.to_string_lossy();
            let mut hooks = FINAL_UNLINK_SWAP_HOOKS
                .lock()
                .expect("final unlink hook mutex");
            if let Some(position) = hooks.iter().position(|hook| {
                hook.device == stat_device_id(&stat)
                    && hook.inode == stat.st_ino
                    && hook.target == target
            }) {
                let hook = hooks.remove(position);
                (hook.action)();
            }
        }
        if !rename_noreplace_raw(
            directory.raw_fd(),
            candidate,
            anchor.as_raw_fd(),
            &slot,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private delete slot already exists",
            ));
        }
        if unsafe { libc::fsync(directory.raw_fd()) } != 0
            || unsafe { libc::fsync(anchor.as_raw_fd()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let final_raw = unsafe {
            libc::openat(
                anchor.as_raw_fd(),
                slot.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if final_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let final_file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(final_raw) });
        let final_identity = metadata_identity(&final_file.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&final_identity) {
            // This can only be reached by the synthetic same-uid pre-entry
            // failpoint. Restore the observed winner to the deterministic
            // quarantine when possible and preserve both objects otherwise.
            let restored = rename_noreplace_raw(
                anchor.as_raw_fd(),
                &slot,
                directory.raw_fd(),
                &quarantine,
            )?;
            sync_rename_parents_raw(anchor.as_raw_fd(), directory.raw_fd())?;
            return Err(io::Error::other(if restored {
                "delete candidate was swapped after its final check; preserved the winner in the deterministic quarantine"
            } else {
                "delete candidate was swapped after its final check; preserved the winner in the private delete anchor"
            }));
        }
        if unsafe { libc::unlinkat(anchor.as_raw_fd(), slot.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fsync(anchor.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("joining identity-conditioned delete: {error}"))?
    .map_err(|error| format!("identity-conditioned delete failed: {error}"))
}

fn delete_quarantine_name(name: &str) -> String {
    format!(".plurx-delete-{name}")
}

fn final_delete_quarantine_name(name: &str) -> String {
    format!("{}.removing", delete_quarantine_name(name))
}

enum ProofLinkOutcome {
    Matched(LocalMediaIdentity),
    CreatedMismatch(LocalMediaIdentity),
    ExistingMismatch,
}

#[cfg(unix)]
async fn link_public_replacement_proof_once(
    public_parent: &SecureDirectory,
    public_name: &str,
    expected: &LocalMediaIdentity,
    scratch: &SecureDirectory,
) -> Result<ProofLinkOutcome, String> {
    let public_parent = public_parent.clone();
    let public_name = public_name.to_owned();
    let expected = expected.clone();
    let scratch = scratch.clone();
    tokio::task::spawn_blocking(move || -> io::Result<ProofLinkOutcome> {
        let public_name = CString::new(public_name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "public name has NUL"))?;
        let proof_name = CString::new(PUBLIC_PROOF_FILE).expect("static proof name");
        let public_raw = unsafe {
            libc::openat(
                public_parent.raw_fd(),
                public_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if public_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let public = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(public_raw) });
        if metadata_identity(&public.metadata()?).map_err(io::Error::other)? != expected {
            return Err(io::Error::other(
                "public replacement changed before hard-link proof creation",
            ));
        }
        #[cfg(test)]
        {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(scratch.raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            let mut hook = PROOF_CREATION_SWAP_HOOK
                .lock()
                .expect("proof creation hook mutex");
            if hook.as_ref().is_some_and(|hook| {
                hook.device == stat_device_id(&stat) && hook.inode == stat.st_ino
            }) {
                let hook = hook.take().expect("matched proof creation hook exists");
                (hook.action)();
            }
        }
        let created = if unsafe {
            libc::linkat(
                public_parent.raw_fd(),
                public_name.as_ptr(),
                scratch.raw_fd(),
                proof_name.as_ptr(),
                0,
            )
        } == 0
        {
            true
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            false
        };
        let proof_raw = unsafe {
            libc::openat(
                scratch.raw_fd(),
                proof_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if proof_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let proof = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(proof_raw) });
        let proof_identity = metadata_identity(&proof.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&proof_identity) {
            return Ok(if created {
                ProofLinkOutcome::CreatedMismatch(proof_identity)
            } else {
                ProofLinkOutcome::ExistingMismatch
            });
        }
        public.sync_all()?;
        if unsafe { libc::fsync(scratch.raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ProofLinkOutcome::Matched(proof_identity))
    })
    .await
    .map_err(|error| format!("joining public replacement proof creation: {error}"))?
    .map_err(|error| format!("creating public replacement proof: {error}"))
}

#[cfg(unix)]
async fn ensure_public_replacement_proof(
    public_parent: &SecureDirectory,
    public_name: &str,
    expected: &LocalMediaIdentity,
    scratch: &SecureDirectory,
) -> Result<LocalMediaIdentity, String> {
    // linkat(2) names its source rather than accepting the already-open file
    // descriptor on all supported platforms. If that pathname is swapped in
    // the interval, reject and identity-conditionally remove the poisoned
    // no-replace destination while the original remains quarantined. A
    // restored public pathname can therefore retry instead of being blocked
    // forever by the attacker's EEXIST artifact.
    for attempt in 0..2 {
        match link_public_replacement_proof_once(public_parent, public_name, expected, scratch)
            .await?
        {
            ProofLinkOutcome::Matched(identity) => return Ok(identity),
            ProofLinkOutcome::CreatedMismatch(identity) => {
                unlink_expected_child(scratch, PUBLIC_PROOF_FILE, &identity).await?;
                if attempt == 1 {
                    return Err(
                        "public replacement changed repeatedly during hard-link proof creation"
                            .to_owned(),
                    );
                }
            }
            ProofLinkOutcome::ExistingMismatch => {
                return Err("refusing unrelated pre-existing replacement recovery guard".to_owned());
            }
        }
    }
    unreachable!("bounded proof creation loop returns")
}

#[cfg(unix)]
async fn restore_public_from_recovery_guard(
    scratch: &SecureDirectory,
    expected: &LocalMediaIdentity,
    public_parent: &SecureDirectory,
    public_name: &str,
) -> Result<LocalMediaIdentity, String> {
    let scratch = scratch.clone();
    let expected = expected.clone();
    let public_parent = public_parent.clone();
    let public_name = public_name.to_owned();
    let scratch_for_link = scratch.clone();
    let public_parent_for_link = public_parent.clone();
    let public_name_for_link = public_name.clone();
    let outcome = tokio::task::spawn_blocking(move || -> io::Result<ProofLinkOutcome> {
        let guard_name = CString::new(PUBLIC_PROOF_FILE).expect("static guard name");
        let public_name = CString::new(public_name_for_link)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "public name has NUL"))?;
        let guard_raw = unsafe {
            libc::openat(
                scratch_for_link.raw_fd(),
                guard_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if guard_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let guard = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(guard_raw) });
        if metadata_identity(&guard.metadata()?).map_err(io::Error::other)? != expected {
            return Err(io::Error::other(
                "recovery guard changed before public restoration",
            ));
        }
        if unsafe {
            libc::linkat(
                scratch_for_link.raw_fd(),
                guard_name.as_ptr(),
                public_parent_for_link.raw_fd(),
                public_name.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        #[cfg(test)]
        {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(scratch_for_link.raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            let mut hook = PUBLIC_RESTORE_SWAP_HOOK
                .lock()
                .expect("public restore hook mutex");
            if hook.as_ref().is_some_and(|hook| {
                hook.device == stat_device_id(&stat) && hook.inode == stat.st_ino
            }) {
                let hook = hook.take().expect("matched public restore hook exists");
                (hook.action)();
            }
        }
        let public_raw = unsafe {
            libc::openat(
                public_parent_for_link.raw_fd(),
                public_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if public_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let public = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(public_raw) });
        let public_identity = metadata_identity(&public.metadata()?).map_err(io::Error::other)?;
        if !expected.same_inode_and_content_facts(&public_identity) {
            return Ok(ProofLinkOutcome::CreatedMismatch(public_identity));
        }
        guard.sync_all()?;
        sync_rename_parents(&scratch_for_link, &public_parent_for_link)?;
        Ok(ProofLinkOutcome::Matched(public_identity))
    })
    .await
    .map_err(|error| format!("joining public recovery restoration: {error}"))?
    .map_err(|error| format!("restoring public replacement from recovery guard: {error}"))?;
    match outcome {
        ProofLinkOutcome::Matched(identity) => Ok(identity),
        ProofLinkOutcome::CreatedMismatch(_) => Err(
            "public pathname changed during recovery restoration; preserving the observed winner and durable recovery guard"
                .to_owned(),
        ),
        ProofLinkOutcome::ExistingMismatch => unreachable!("public restoration is no-replace"),
    }
}

#[cfg(unix)]
async fn unlink_original_if_public_matches(
    scratch: &SecureDirectory,
    pending_name: &str,
    expected_pending: &LocalMediaIdentity,
    public_parent: &SecureDirectory,
    public_name: &str,
    expected_public: &LocalMediaIdentity,
) -> Result<Option<LocalMediaIdentity>, String> {
    let scratch = scratch.clone();
    let public_parent = public_parent.clone();
    let pending_name = pending_name.to_owned();
    let public_name = public_name.to_owned();
    let expected_pending = expected_pending.clone();
    let expected_public = expected_public.clone();
    let blocking_scratch = scratch.clone();
    let blocking_public_parent = public_parent.clone();
    let blocking_pending_name = pending_name.clone();
    let blocking_public_name = public_name.clone();
    let blocking_expected_pending = expected_pending.clone();
    let blocking_expected_public = expected_public.clone();
    let checked_public =
        tokio::task::spawn_blocking(move || -> io::Result<Option<LocalMediaIdentity>> {
            #[cfg(test)]
            let hook_target = blocking_pending_name.clone();
            let pending_name = CString::new(blocking_pending_name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pending name has NUL"))?;
            let public_name = CString::new(blocking_public_name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "public name has NUL"))?;
            let pending_present = raw_child_exists(blocking_scratch.raw_fd(), &pending_name)?;
            if pending_present {
                let pending_raw = unsafe {
                    libc::openat(
                        blocking_scratch.raw_fd(),
                        pending_name.as_ptr(),
                        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    )
                };
                if pending_raw < 0 {
                    return Err(io::Error::last_os_error());
                }
                let pending = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(pending_raw) });
                let actual = metadata_identity(&pending.metadata()?).map_err(io::Error::other)?;
                if !blocking_expected_pending.same_inode_and_content_facts(&actual) {
                    return Err(io::Error::other(
                        "pending original no longer has the expected local identity",
                    ));
                }
            }
            // The public child is reopened and checked in the same non-awaiting
            // capability operation as the final unlink. A changed pathname leaves
            // the durable deterministic original quarantine untouched.
            let public_raw = unsafe {
                libc::openat(
                    blocking_public_parent.raw_fd(),
                    public_name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if public_raw < 0 {
                return Ok(None);
            }
            let public = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(public_raw) });
            if metadata_identity(&public.metadata()?).map_err(io::Error::other)?
                != blocking_expected_public
            {
                return Ok(None);
            }
            #[cfg(test)]
            {
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstat(blocking_scratch.raw_fd(), stat.as_mut_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let stat = unsafe { stat.assume_init() };
                let mut hooks = FINALIZE_SWAP_HOOKS.lock().expect("finalize hook mutex");
                if let Some(position) = hooks.iter().position(|hook| {
                    hook.device == stat_device_id(&stat)
                        && hook.inode == stat.st_ino
                        && hook.target == hook_target
                }) {
                    let hook = hooks.remove(position);
                    (hook.action)();
                }
            }
            Ok(Some(
                metadata_identity(&public.metadata()?).map_err(io::Error::other)?,
            ))
        })
        .await
        .map_err(|error| format!("joining committed-original finalization: {error}"))?
        .map_err(|error| format!("finalizing committed original: {error}"))?;
    let Some(checked_public) = checked_public else {
        return Ok(None);
    };
    unlink_expected_child(&scratch, &pending_name, &expected_pending).await?;
    let public_after = match child_identity(&public_parent, &public_name).await {
        Ok(identity) => identity,
        Err(_) => return Ok(None),
    };
    if !expected_public.same_inode_and_content_facts(&public_after) {
        return Ok(None);
    }
    Ok(Some(
        if checked_public.same_inode_and_content_facts(&public_after) {
            public_after
        } else {
            return Ok(None);
        },
    ))
}

#[cfg(unix)]
async fn remove_flat_tree_expected(
    parent: &SecureDirectory,
    name: &str,
    directory: &SecureDirectory,
    expected: plurx_core::fs_secure::FileIdentity,
    loss: &CancellationToken,
) -> Result<(), String> {
    let mut names = directory
        .child_names(MAX_SCRATCH_ENTRIES)
        .await
        .map_err(|error| format!("listing owned scratch for bounded cleanup: {error}"))?;
    names.sort_by_key(|name| name == SCRATCH_OWNER_FILE);
    for child in names {
        if child == PRIVATE_DELETE_ANCHOR {
            continue;
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost during bounded scratch cleanup".to_owned());
        }
        let metadata = directory
            .child_metadata(&child)
            .await
            .map_err(|error| format!("inspecting owned scratch child {child}: {error}"))?;
        if !metadata.is_file {
            return Err(format!(
                "refusing nested or special scratch child during cleanup: {child}"
            ));
        }
        let local = child_identity(directory, &child).await?;
        unlink_expected_child(directory, &child, &local).await?;
    }
    drain_private_delete_anchor(directory, loss).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup-root removal".to_owned());
    }
    let parent = parent.clone();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "tree name has NUL"))?;
        let legacy_final = CString::new(format!("{}.removing", name.to_string_lossy()))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final tree name has NUL"))?;
        let slot = CString::new(private_delete_slot(
            &name.to_string_lossy(),
            PrivateDeleteKind::Directory,
        ))
        .expect("hashed private directory slot has no NUL");
        let anchor = open_private_delete_anchor_raw(parent.raw_fd(), true)?
            .expect("create=true returns an anchor");

        if raw_child_exists(anchor.as_raw_fd(), &slot)? {
            let raw = unsafe {
                libc::openat(
                    anchor.as_raw_fd(),
                    slot.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let held = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let metadata = held.metadata()?;
            if metadata.dev() != expected.device || metadata.ino() != expected.inode {
                return Err(io::Error::other(
                    "private cleanup slot does not contain the expected root",
                ));
            }
            remove_inner_private_anchor(&held)?;
            if unsafe { libc::unlinkat(anchor.as_raw_fd(), slot.as_ptr(), libc::AT_REMOVEDIR) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::fsync(anchor.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }

        let source = match (
            raw_child_exists(parent.raw_fd(), &name)?,
            raw_child_exists(parent.raw_fd(), &legacy_final)?,
        ) {
            (true, false) => &name,
            (false, true) => &legacy_final,
            _ => {
                return Err(io::Error::other(
                    "cleanup root has an ambiguous or absent recovery namespace",
                ));
            }
        };
        let raw = unsafe {
            libc::openat(
                parent.raw_fd(),
                source.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let held = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let metadata = held.metadata()?;
        if metadata.dev() != expected.device || metadata.ino() != expected.inode {
            return Err(io::Error::other(
                "cleanup root no longer has the quarantined identity",
            ));
        }
        #[cfg(test)]
        {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(parent.raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            let target = source.to_string_lossy();
            let mut hooks = FINAL_RMDIR_SWAP_HOOKS
                .lock()
                .expect("final rmdir hook mutex");
            if let Some(position) = hooks.iter().position(|hook| {
                hook.device == stat_device_id(&stat)
                    && hook.inode == stat.st_ino
                    && hook.target == target
            }) {
                let hook = hooks.remove(position);
                (hook.action)();
            }
        }
        if !rename_noreplace_raw(parent.raw_fd(), source, anchor.as_raw_fd(), &slot)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private cleanup slot already exists",
            ));
        }
        if unsafe { libc::fsync(parent.raw_fd()) } != 0
            || unsafe { libc::fsync(anchor.as_raw_fd()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let final_raw = unsafe {
            libc::openat(
                anchor.as_raw_fd(),
                slot.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if final_raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let final_directory = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(final_raw) });
        let final_metadata = final_directory.metadata()?;
        if final_metadata.dev() != expected.device || final_metadata.ino() != expected.inode {
            let restored = rename_noreplace_raw(
                anchor.as_raw_fd(),
                &slot,
                parent.raw_fd(),
                source,
            )?;
            sync_rename_parents_raw(anchor.as_raw_fd(), parent.raw_fd())?;
            return Err(io::Error::other(if restored {
                "cleanup root was swapped after its final check; preserved the winner in the durable quarantine"
            } else {
                "cleanup root was swapped after its final check; preserved the winner in the private cleanup anchor"
            }));
        }
        remove_inner_private_anchor(&final_directory)?;
        if unsafe { libc::unlinkat(anchor.as_raw_fd(), slot.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fsync(anchor.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("joining identity-conditioned tree removal: {error}"))?
    .map_err(|error| format!("identity-conditioned tree removal failed: {error}"))
}

#[cfg(windows)]
fn local_identity_from_secure(identity: plurx_core::fs_secure::FileIdentity) -> LocalMediaIdentity {
    LocalMediaIdentity {
        device: identity.device,
        inode: identity.inode ^ identity.inode_high.rotate_left(1),
        size: identity.size,
        modified_seconds: identity.changed_seconds,
        modified_nanoseconds: identity.changed_nanoseconds,
        changed_seconds: identity.changed_seconds,
        changed_nanoseconds: identity.changed_nanoseconds,
    }
}

#[cfg(windows)]
async fn create_cleanup_state(
    parent: &SecureDirectory,
    paths: &ConversionPaths,
    state: &CleanupQuarantineState,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("encoding cleanup quarantine state: {error}"))?;
    if bytes.len() as u64 > MAX_CLEANUP_STATE_BYTES {
        return Err("cleanup quarantine state exceeds its bounded format".to_owned());
    }
    parent
        .create_new_child(&paths.cleanup_state_name, &bytes)
        .await
        .map_err(|error| format!("creating exclusive cleanup state: {error}"))
}

#[cfg(windows)]
async fn sync_directory(path: &Path) -> Result<(), String> {
    let directory = SecureDirectory::open(path)
        .await
        .map_err(|error| format!("opening directory {} for sync: {error}", path.display()))?;
    directory
        .sync()
        .await
        .map_err(|error| format!("syncing directory {}: {error}", path.display()))
}

#[cfg(all(test, windows))]
async fn rename_noreplace_durable(from: &Path, to: &Path) -> Result<bool, String> {
    let from_parent = open_parent(from, "rename source").await?;
    let to_parent = open_parent(to, "rename destination").await?;
    from_parent
        .rename_child_to_noreplace(
            &path_child_name(from, "rename source")?,
            &to_parent,
            &path_child_name(to, "rename destination")?,
        )
        .await
        .map_err(|error| format!("exclusive capability rename failed: {error}"))
}

#[cfg(windows)]
async fn rename_expected_noreplace_between(
    from_parent: &SecureDirectory,
    from_name: &str,
    expected: &LocalMediaIdentity,
    to_parent: &SecureDirectory,
    to_name: &str,
) -> Result<Option<LocalMediaIdentity>, String> {
    let before = child_identity(from_parent, from_name).await?;
    if &before != expected {
        return Err("rename source no longer has the expected local identity".to_owned());
    }
    if !from_parent
        .rename_child_to_noreplace(from_name, to_parent, to_name)
        .await
        .map_err(|error| format!("identity-conditioned rename failed: {error}"))?
    {
        return Ok(None);
    }
    let after = child_identity(to_parent, to_name).await?;
    if !expected.same_inode_and_content_facts(&after) {
        let _ = to_parent
            .rename_child_to_noreplace(to_name, from_parent, from_name)
            .await;
        return Err("rename source changed during the capability operation".to_owned());
    }
    Ok(Some(after))
}

#[cfg(windows)]
async fn private_delete_pending(
    _directory: &SecureDirectory,
    _name: &str,
    _kind: PrivateDeleteKind,
) -> Result<bool, String> {
    Ok(false)
}

#[cfg(windows)]
async fn restore_private_delete_slot(
    _directory: &SecureDirectory,
    _name: &str,
    _kind: PrivateDeleteKind,
) -> Result<bool, String> {
    Ok(false)
}

#[cfg(windows)]
async fn drain_private_delete_anchor(
    _directory: &SecureDirectory,
    _loss: &CancellationToken,
) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
async fn unlink_expected_child(
    directory: &SecureDirectory,
    name: &str,
    expected: &LocalMediaIdentity,
) -> Result<(), String> {
    let removed = directory
        .unlink_child_if_identity(
            name,
            expected.device,
            expected.inode,
            expected.size,
            expected.changed_seconds,
            expected.changed_nanoseconds,
        )
        .await
        .map_err(|error| format!("identity-conditioned delete failed: {error}"))?;
    if removed {
        Ok(())
    } else {
        Err("delete candidate no longer has the expected local identity".to_owned())
    }
}

#[cfg(windows)]
async fn ensure_public_replacement_proof(
    public_parent: &SecureDirectory,
    public_name: &str,
    expected: &LocalMediaIdentity,
    scratch: &SecureDirectory,
) -> Result<LocalMediaIdentity, String> {
    if child_identity(public_parent, public_name).await? != *expected {
        return Err("public replacement changed before proof creation".to_owned());
    }
    scratch
        .place_regular_child_from(public_parent, public_name, PUBLIC_PROOF_FILE, expected.size)
        .await
        .map_err(|error| format!("creating Windows replacement proof copy: {error}"))?;
    if child_identity(public_parent, public_name).await? != *expected {
        return Err("public replacement changed during proof creation".to_owned());
    }
    child_identity(scratch, PUBLIC_PROOF_FILE).await
}

#[cfg(windows)]
async fn restore_public_from_recovery_guard(
    scratch: &SecureDirectory,
    expected: &LocalMediaIdentity,
    public_parent: &SecureDirectory,
    public_name: &str,
) -> Result<LocalMediaIdentity, String> {
    if child_identity(scratch, PUBLIC_PROOF_FILE).await? != *expected {
        return Err("recovery guard changed before public restoration".to_owned());
    }
    let staging_name = format!(".plurx-restore-{}.tmp", uuid::Uuid::new_v4());
    public_parent
        .place_regular_child_from(scratch, PUBLIC_PROOF_FILE, &staging_name, expected.size)
        .await
        .map_err(|error| format!("staging public replacement from recovery guard: {error}"))?;
    let installed = public_parent
        .rename_child_noreplace(&staging_name, public_name)
        .await
        .map_err(|error| format!("restoring public replacement atomically: {error}"));
    match installed {
        Ok(true) => {}
        Ok(false) => {
            let _ = public_parent.unlink_child(&staging_name).await;
            return Err("public replacement reappeared before atomic restoration".to_owned());
        }
        Err(error) => {
            let _ = public_parent.unlink_child(&staging_name).await;
            return Err(error);
        }
    }
    public_parent
        .sync()
        .await
        .map_err(|error| format!("syncing atomically restored replacement: {error}"))?;
    child_identity(public_parent, public_name).await
}

#[cfg(windows)]
async fn unlink_original_if_public_matches(
    scratch: &SecureDirectory,
    pending_name: &str,
    expected_pending: &LocalMediaIdentity,
    public_parent: &SecureDirectory,
    public_name: &str,
    expected_public: &LocalMediaIdentity,
) -> Result<Option<LocalMediaIdentity>, String> {
    if child_identity(public_parent, public_name).await? != *expected_public {
        return Ok(None);
    }
    unlink_expected_child(scratch, pending_name, expected_pending).await?;
    let after = child_identity(public_parent, public_name).await?;
    Ok(expected_public
        .same_inode_and_content_facts(&after)
        .then_some(after))
}

#[cfg(windows)]
async fn remove_flat_tree_expected(
    parent: &SecureDirectory,
    name: &str,
    directory: &SecureDirectory,
    expected: plurx_core::fs_secure::FileIdentity,
    loss: &CancellationToken,
) -> Result<(), String> {
    let actual = directory
        .identity()
        .await
        .map_err(|error| format!("identifying cleanup root: {error}"))?;
    if !expected.same_inode(actual) {
        return Err("cleanup root no longer has the quarantined identity".to_owned());
    }
    let mut names = directory
        .child_names(MAX_SCRATCH_ENTRIES)
        .await
        .map_err(|error| format!("listing owned scratch for bounded cleanup: {error}"))?;
    names.sort_by_key(|child| child == SCRATCH_OWNER_FILE);
    for child in names {
        if loss.is_cancelled() {
            return Err("conversion lease was lost during bounded scratch cleanup".to_owned());
        }
        let identity = child_identity(directory, &child).await?;
        unlink_expected_child(directory, &child, &identity).await?;
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before cleanup-root removal".to_owned());
    }
    parent
        .remove_child_tree(name, MAX_SCRATCH_ENTRIES, 1)
        .await
        .map_err(|error| format!("removing empty Windows cleanup root: {error}"))
}

const FAILED_PUBLISHED_FILE: &str = "failed-published.mkv";

struct PublicationContext {
    scratch: OwnedScratch,
    source_parent: SecureDirectory,
    source_name: String,
    retained_name: String,
    replacement: PersistedMediaIdentity,
    expected_bytes: i64,
    keep_original: bool,
    node_id: String,
}

async fn publication_context(
    paths: &ConversionPaths,
    file: &MediaFile,
    node_id: &str,
    expected_bytes: i64,
) -> Result<PublicationContext, String> {
    let scratch = require_owned_scratch(paths, file, None).await?;
    if scratch.owner.expected_bytes != Some(expected_bytes) {
        return Err(
            "verified ledger byte count does not match the owned scratch manifest".to_owned(),
        );
    }
    let replacement = scratch
        .owner
        .replacement
        .clone()
        .ok_or_else(|| "owned scratch has no verified replacement identity".to_owned())?;
    if replacement.content.size != expected_bytes.max(0) as u64 {
        return Err("verified replacement identity has the wrong byte count".to_owned());
    }
    let keep_original = scratch
        .owner
        .keep_original
        .ok_or_else(|| "owned scratch has no persisted retention decision".to_owned())?;
    Ok(PublicationContext {
        scratch,
        source_parent: open_parent(&file.path, "media source").await?,
        source_name: path_child_name(&file.path, "media source")?,
        retained_name: path_child_name(&paths.retained_original, "retained original")?,
        replacement,
        expected_bytes,
        keep_original,
        node_id: node_id.to_owned(),
    })
}

impl PublicationContext {
    async fn bind_source(
        &mut self,
        directory: &SecureDirectory,
        name: &str,
        loss: &CancellationToken,
        role: &str,
    ) -> Result<BoundMedia, String> {
        let expected = self.scratch.owner.source.clone();
        let (bound, rebound) =
            bind_expected_content(directory, name, &expected, &self.node_id, loss, role).await?;
        if rebound {
            self.scratch
                .owner
                .source
                .rebaseline(&self.node_id, bound.local.clone());
            write_scratch_owner(&self.scratch.directory, &self.scratch.owner).await?;
        }
        Ok(bound)
    }

    async fn bind_replacement(
        &mut self,
        directory: &SecureDirectory,
        name: &str,
        loss: &CancellationToken,
        role: &str,
    ) -> Result<BoundMedia, String> {
        let expected = self.replacement.clone();
        let (bound, rebound) =
            bind_expected_content(directory, name, &expected, &self.node_id, loss, role).await?;
        if rebound {
            self.replacement
                .rebaseline(&self.node_id, bound.local.clone());
            self.scratch.owner.replacement = Some(self.replacement.clone());
            write_scratch_owner(&self.scratch.directory, &self.scratch.owner).await?;
        }
        Ok(bound)
    }

    async fn rebaseline_source(&mut self, local: LocalMediaIdentity) -> Result<(), String> {
        self.scratch.owner.source.rebaseline(&self.node_id, local);
        write_scratch_owner(&self.scratch.directory, &self.scratch.owner).await
    }

    async fn rebaseline_replacement(&mut self, local: LocalMediaIdentity) -> Result<(), String> {
        self.replacement.rebaseline(&self.node_id, local);
        self.scratch.owner.replacement = Some(self.replacement.clone());
        write_scratch_owner(&self.scratch.directory, &self.scratch.owner).await
    }

    async fn bind_restore(
        &mut self,
        directory: &SecureDirectory,
        name: &str,
        loss: &CancellationToken,
        role: &str,
    ) -> Result<BoundMedia, String> {
        let expected = self
            .scratch
            .owner
            .rollback_restore
            .clone()
            .ok_or_else(|| "rollback restoration identity is missing".to_owned())?;
        let (bound, rebound) =
            bind_expected_content(directory, name, &expected, &self.node_id, loss, role).await?;
        if rebound {
            self.scratch
                .owner
                .rollback_restore
                .as_mut()
                .expect("checked above")
                .rebaseline(&self.node_id, bound.local.clone());
            write_scratch_owner(&self.scratch.directory, &self.scratch.owner).await?;
        }
        Ok(bound)
    }
}

async fn retain_original_before_commit(
    paths: &ConversionPaths,
    context: &mut PublicationContext,
    loss: &CancellationToken,
) -> Result<Option<String>, String> {
    if !context.keep_original {
        return Ok(None);
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before retaining the original".to_owned());
    }
    let staged_exists = child_exists(&context.scratch.directory, "source.p7.original").await?;
    if staged_exists {
        let scratch_directory = context.scratch.directory.clone();
        let staged = context
            .bind_source(
                &scratch_directory,
                "source.p7.original",
                loss,
                "staged original for retention",
            )
            .await?;
        let Some(retained_local) = rename_expected_noreplace_between(
            &scratch_directory,
            "source.p7.original",
            &staged.local,
            &context.source_parent,
            &context.retained_name,
        )
        .await?
        else {
            return Err(format!(
                "refusing to overwrite retained original {}",
                paths.retained_original.display()
            ));
        };
        context.rebaseline_source(retained_local).await?;
    }
    let source_parent = context.source_parent.clone();
    let retained_name = context.retained_name.clone();
    context
        .bind_source(&source_parent, &retained_name, loss, "retained original")
        .await?;
    Ok(Some(
        paths
            .retained_original
            .to_str()
            .ok_or_else(|| "retained original path is not valid UTF-8".to_owned())?
            .to_owned(),
    ))
}

async fn finalize_verified_original_inner(
    paths: &ConversionPaths,
    context: &mut PublicationContext,
    loss: &CancellationToken,
) -> Result<Option<String>, String> {
    if loss.is_cancelled() {
        return Err("conversion lease was lost before verified original finalization".to_owned());
    }
    if context.keep_original {
        let source_parent = context.source_parent.clone();
        let retained_name = context.retained_name.clone();
        context
            .bind_source(
                &source_parent,
                &retained_name,
                loss,
                "verified retained original before ledger commit",
            )
            .await?;
        return persisted_original_path(paths, context);
    }
    let scratch_directory = context.scratch.directory.clone();
    if child_exists(&scratch_directory, "source.p7.original").await? {
        let staged = context
            .bind_source(
                &scratch_directory,
                "source.p7.original",
                loss,
                "staged original for verified quarantine",
            )
            .await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before original quarantine rename".to_owned());
        }
        let Some(pending_local) = rename_expected_noreplace_between(
            &scratch_directory,
            "source.p7.original",
            &staged.local,
            &scratch_directory,
            DISCARD_PENDING_FILE,
        )
        .await?
        else {
            return Err("verified original quarantine already exists".to_owned());
        };
        context
            .scratch
            .owner
            .source
            .rebaseline(&context.node_id, pending_local);
        context.scratch.owner.discard_pending = true;
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before recording original quarantine".to_owned(),
            );
        }
        write_scratch_owner(&scratch_directory, &context.scratch.owner).await?;
    }
    let proof = context
        .bind_replacement(
            &scratch_directory,
            PUBLIC_PROOF_FILE,
            loss,
            "durable replacement recovery guard",
        )
        .await?;
    let source_parent = context.source_parent.clone();
    let source_name = context.source_name.clone();
    let public = context
        .bind_replacement(
            &source_parent,
            &source_name,
            loss,
            "public replacement before verified original deletion",
        )
        .await?;
    if proof.local.device != public.local.device || proof.local.inode != public.local.inode {
        return Err(
            "public pathname changed; retained the original and durable replacement guard"
                .to_owned(),
        );
    }
    if logical_child_exists(&scratch_directory, DISCARD_PENDING_FILE).await? {
        let pending_local = if child_exists(&scratch_directory, DISCARD_PENDING_FILE).await? {
            context
                .bind_source(
                    &scratch_directory,
                    DISCARD_PENDING_FILE,
                    loss,
                    "verified quarantined original",
                )
                .await?
                .local
        } else {
            context.scratch.owner.source.local.identity.clone()
        };
        if loss.is_cancelled() {
            return Err("conversion lease was lost before verified original deletion".to_owned());
        }
        let Some(public_after_unlink) = unlink_original_if_public_matches(
            &scratch_directory,
            DISCARD_PENDING_FILE,
            &pending_local,
            &source_parent,
            &source_name,
            &proof.local,
        )
        .await?
        else {
            return Err(
                "public replacement after original deletion changed; retained the replacement guard"
                    .to_owned(),
            );
        };
        context.rebaseline_replacement(public_after_unlink).await?;
        let proof_after = context
            .bind_replacement(
                &scratch_directory,
                PUBLIC_PROOF_FILE,
                loss,
                "replacement guard after original deletion",
            )
            .await?;
        let public_after = context
            .bind_replacement(
                &source_parent,
                &source_name,
                loss,
                "public replacement after original deletion",
            )
            .await?;
        if proof_after.local.device != public_after.local.device
            || proof_after.local.inode != public_after.local.inode
        {
            return Err(
                "public pathname changed during original deletion; retained the replacement guard"
                    .to_owned(),
            );
        }
    }
    if context.scratch.owner.discard_pending {
        context.scratch.owner.discard_pending = false;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before finalizing original deletion".to_owned());
        }
        write_scratch_owner(&scratch_directory, &context.scratch.owner).await?;
    }
    Ok(None)
}

pub async fn finalize_verified_original(
    file: &MediaFile,
    node_id: &str,
    expected_bytes: i64,
    loss: &CancellationToken,
) -> Result<Option<String>, String> {
    let paths = ConversionPaths::for_file(file)?;
    let mut context = publication_context(&paths, file, node_id, expected_bytes).await?;
    finalize_verified_original_inner(&paths, &mut context, loss).await
}

pub async fn recovery_guard_intent(
    file: &MediaFile,
) -> Result<Option<RecoveryGuardIntent>, String> {
    let paths = ConversionPaths::for_file(file)?;
    let scratch = require_owned_scratch(&paths, file, None).await?;
    if scratch.owner.keep_original != Some(false) {
        return Ok(None);
    }
    let guard_id = scratch
        .owner
        .recovery_guard_id
        .clone()
        .ok_or_else(|| "discarding conversion has no persisted recovery guard id".to_owned())?;
    Ok(Some(RecoveryGuardIntent {
        guard_id,
        recovery_path: paths
            .directory
            .join(PUBLIC_PROOF_FILE)
            .to_str()
            .ok_or_else(|| "replacement recovery guard path is not valid UTF-8".to_owned())?
            .to_owned(),
    }))
}

fn persisted_original_path(
    paths: &ConversionPaths,
    context: &PublicationContext,
) -> Result<Option<String>, String> {
    context
        .keep_original
        .then(|| {
            paths
                .retained_original
                .to_str()
                .ok_or_else(|| "retained original path is not valid UTF-8".to_owned())
                .map(str::to_owned)
        })
        .transpose()
}

async fn rollback_published(
    _file: &MediaFile,
    context: &mut PublicationContext,
    loss: &CancellationToken,
    reason: String,
) -> Result<PublicationOutcome, String> {
    if loss.is_cancelled() {
        return Err("conversion lease was lost before rollback".to_owned());
    }
    if context.scratch.owner.rollback_restore.is_none() {
        context.scratch.owner.rollback_restore = Some(context.scratch.owner.source.clone());
    }
    context.scratch.owner.rollback_reason = Some(reason.clone());
    write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;

    let public_exists = child_exists(&context.source_parent, &context.source_name).await?;
    let quarantined_exists =
        child_exists(&context.scratch.directory, FAILED_PUBLISHED_FILE).await?;
    let staged_exists = child_exists(&context.scratch.directory, "source.p7.original").await?;
    let replacement_exists = child_exists(&context.scratch.directory, "replacement.mkv").await?;
    let retained_exists = child_exists(&context.source_parent, &context.retained_name).await?;
    let expected_restore = context
        .scratch
        .owner
        .rollback_restore
        .as_ref()
        .expect("rollback restoration identity was persisted")
        .clone();
    let staged = if staged_exists {
        let directory = context.scratch.directory.clone();
        Some(
            context
                .bind_restore(
                    &directory,
                    "source.p7.original",
                    loss,
                    "rollback staged original",
                )
                .await?,
        )
    } else {
        None
    };
    let retained = if retained_exists {
        let directory = context.source_parent.clone();
        let retained_name = context.retained_name.clone();
        Some(
            context
                .bind_restore(
                    &directory,
                    &retained_name,
                    loss,
                    "rollback retained original",
                )
                .await?,
        )
    } else {
        None
    };
    if public_exists {
        let (public, public_is_restore, public_rebound) = bind_one_of(
            &context.source_parent,
            &context.source_name,
            &expected_restore,
            &context.replacement,
            &context.node_id,
            loss,
            "public child during rollback",
        )
        .await?;
        if public_rebound {
            if public_is_restore {
                context
                    .scratch
                    .owner
                    .rollback_restore
                    .as_mut()
                    .expect("rollback identity exists")
                    .rebaseline(&context.node_id, public.local.clone());
            } else {
                context
                    .replacement
                    .rebaseline(&context.node_id, public.local.clone());
                context.scratch.owner.replacement = Some(context.replacement.clone());
            }
            write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;
        }
        if public_is_restore && staged.is_none() {
            if quarantined_exists {
                let directory = context.scratch.directory.clone();
                let quarantined = context
                    .bind_replacement(
                        &directory,
                        FAILED_PUBLISHED_FILE,
                        loss,
                        "rollback replacement quarantine",
                    )
                    .await?;
                if loss.is_cancelled() {
                    return Err("conversion lease was lost before rollback cleanup".to_owned());
                }
                unlink_expected_child(
                    &context.scratch.directory,
                    FAILED_PUBLISHED_FILE,
                    &quarantined.local,
                )
                .await?;
            }
            if replacement_exists {
                let directory = context.scratch.directory.clone();
                let replacement = context
                    .bind_replacement(&directory, "replacement.mkv", loss, "rollback replacement")
                    .await?;
                if loss.is_cancelled() {
                    return Err("conversion lease was lost before rollback cleanup".to_owned());
                }
                unlink_expected_child(
                    &context.scratch.directory,
                    "replacement.mkv",
                    &replacement.local,
                )
                .await?;
            }
            prune_published_scratch(context, loss).await?;
            return Ok(PublicationOutcome::SafelyRolledBack { reason });
        }
        if public_is_restore {
            return Err(
                "public pathname contains a newer winner; refusing destructive rollback".to_owned(),
            );
        }
        if staged.is_none() && retained.is_none() {
            return Err(
                "rollback has no manifest-bound original to restore; preserving replacement"
                    .to_owned(),
            );
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before quarantining the replacement".to_owned());
        }
        let quarantined_local = if quarantined_exists {
            None
        } else {
            rename_expected_noreplace_between(
                &context.source_parent,
                &context.source_name,
                &public.local,
                &context.scratch.directory,
                FAILED_PUBLISHED_FILE,
            )
            .await?
        };
        let Some(quarantined_local) = quarantined_local else {
            return Err("failed replacement quarantine destination already exists".to_owned());
        };
        context.rebaseline_replacement(quarantined_local).await?;
        let directory = context.scratch.directory.clone();
        context
            .bind_replacement(
                &directory,
                FAILED_PUBLISHED_FILE,
                loss,
                "quarantined published replacement",
            )
            .await?;
    }

    if loss.is_cancelled() {
        return Err("conversion lease was lost before restoring the original".to_owned());
    }
    if staged.is_none() && retained.is_none() {
        return Err("rollback has no manifest-bound original to restore".to_owned());
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before restoring the original".to_owned());
    }
    let (restore_parent, restore_name, restore_local) = if let Some(staged) = staged.as_ref() {
        (
            &context.scratch.directory,
            "source.p7.original",
            &staged.local,
        )
    } else {
        (
            &context.source_parent,
            context.retained_name.as_str(),
            &retained.as_ref().expect("checked above").local,
        )
    };
    let Some(restored_local) = rename_expected_noreplace_between(
        restore_parent,
        restore_name,
        restore_local,
        &context.source_parent,
        &context.source_name,
    )
    .await?
    else {
        return Err("a newer public source appeared while restoring the original".to_owned());
    };
    if let Some(restore) = context.scratch.owner.rollback_restore.as_mut() {
        restore.rebaseline(&context.node_id, restored_local.clone());
    }
    if context.scratch.owner.source.content == expected_restore.content {
        context
            .scratch
            .owner
            .source
            .rebaseline(&context.node_id, restored_local);
    }
    write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;
    let directory = context.source_parent.clone();
    let source_name = context.source_name.clone();
    context
        .bind_restore(&directory, &source_name, loss, "restored source")
        .await?;
    if child_exists(&context.scratch.directory, FAILED_PUBLISHED_FILE).await? {
        let directory = context.scratch.directory.clone();
        let failed = context
            .bind_replacement(
                &directory,
                FAILED_PUBLISHED_FILE,
                loss,
                "rollback quarantine cleanup",
            )
            .await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before rollback cleanup".to_owned());
        }
        unlink_expected_child(
            &context.scratch.directory,
            FAILED_PUBLISHED_FILE,
            &failed.local,
        )
        .await?;
    }
    if child_exists(&context.scratch.directory, "replacement.mkv").await? {
        let directory = context.scratch.directory.clone();
        let replacement = context
            .bind_replacement(
                &directory,
                "replacement.mkv",
                loss,
                "rollback replacement cleanup",
            )
            .await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before rollback cleanup".to_owned());
        }
        unlink_expected_child(
            &context.scratch.directory,
            "replacement.mkv",
            &replacement.local,
        )
        .await?;
    }
    prune_published_scratch(context, loss).await?;
    Ok(PublicationOutcome::SafelyRolledBack { reason })
}

#[cfg(test)]
async fn abort_raced_source_staging(
    context: &mut PublicationContext,
    loss: &CancellationToken,
) -> Result<PublicationOutcome, String> {
    if loss.is_cancelled() {
        return Err("conversion lease was lost after a source staging race".to_owned());
    }
    let staged = bind_child_fresh(
        &context.scratch.directory,
        "source.p7.original",
        loss,
        "raced staged source",
    )
    .await?;
    if context.scratch.owner.source.content == staged.content {
        return Err("source staging race did not contain a distinct source inode".to_owned());
    }
    let reason = "source pathname changed during staging".to_owned();
    context.scratch.owner.rollback_reason = Some(reason.clone());
    context.scratch.owner.rollback_restore = Some(persisted_binding(&staged, &context.node_id));
    if loss.is_cancelled() {
        return Err(
            "conversion lease was lost before recording the source staging race".to_owned(),
        );
    }
    write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before restoring the raced source".to_owned());
    }
    let Some(restored_local) = rename_expected_noreplace_between(
        &context.scratch.directory,
        "source.p7.original",
        &staged.local,
        &context.source_parent,
        &context.source_name,
    )
    .await?
    else {
        return Err(
            "source changed during staging and a newer public winner prevented restoration"
                .to_owned(),
        );
    };
    if let Some(restore) = context.scratch.owner.rollback_restore.as_mut() {
        restore.rebaseline(&context.node_id, restored_local);
    }
    write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;
    if child_exists(&context.scratch.directory, "replacement.mkv").await? {
        let directory = context.scratch.directory.clone();
        let replacement = context
            .bind_replacement(
                &directory,
                "replacement.mkv",
                loss,
                "replacement for raced-source cleanup",
            )
            .await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before raced-source cleanup".to_owned());
        }
        unlink_expected_child(
            &context.scratch.directory,
            "replacement.mkv",
            &replacement.local,
        )
        .await?;
    }
    prune_published_scratch(context, loss).await?;
    Ok(PublicationOutcome::SafelyRolledBack { reason })
}

async fn prune_published_scratch(
    context: &PublicationContext,
    loss: &CancellationToken,
) -> Result<(), String> {
    for name in ["BL_RPU.hevc", "BL_RPU.p81.hevc", "RPU.bin"] {
        if child_exists(&context.scratch.directory, name).await? {
            if loss.is_cancelled() {
                return Err("conversion lease was lost before scratch pruning".to_owned());
            }
            let local = child_identity(&context.scratch.directory, name).await?;
            if loss.is_cancelled() {
                return Err("conversion lease was lost before scratch pruning".to_owned());
            }
            unlink_expected_child(&context.scratch.directory, name, &local).await?;
        }
    }
    Ok(())
}

async fn published_probe_and_metadata(
    file: &MediaFile,
    context: &mut PublicationContext,
    loss: &CancellationToken,
) -> Result<(ProbeResult, BoundMedia), String> {
    let directory = context.source_parent.clone();
    let name = context.source_name.clone();
    let public = context
        .bind_replacement(&directory, &name, loss, "published replacement")
        .await?;
    let probe = probe_bound(&public, &file.path, "published replacement", loss).await?;
    if probe.dolby_vision.profile != Some(8) || probe.dolby_vision.el_present != Some(false) {
        return Err(
            "published path does not contain the verified Profile 8 replacement".to_owned(),
        );
    }
    Ok((probe, public))
}

pub async fn publish_verified(
    file: &MediaFile,
    node_id: &str,
    loss: &CancellationToken,
    expected_bytes: i64,
) -> Result<PublicationOutcome, String> {
    let paths = ConversionPaths::for_file(file)?;
    let mut context = publication_context(&paths, file, node_id, expected_bytes).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before publication".to_owned());
    }
    if context.keep_original && child_exists(&context.source_parent, &context.retained_name).await?
    {
        return Err(format!(
            "refusing to overwrite retained original {}",
            paths.retained_original.display()
        ));
    }

    let source_exists = child_exists(&context.source_parent, &context.source_name).await?;
    let staged_exists = child_exists(&context.scratch.directory, "source.p7.original").await?;
    let replacement_exists = child_exists(&context.scratch.directory, "replacement.mkv").await?;
    if source_exists && staged_exists && replacement_exists {
        return Err(
            "source, staged original, and replacement all exist; refusing ambiguous publication"
                .to_owned(),
        );
    }

    if source_exists && !staged_exists {
        let source_parent = context.source_parent.clone();
        let source_name = context.source_name.clone();
        let current_source = context
            .bind_source(
                &source_parent,
                &source_name,
                loss,
                "public source before staging",
            )
            .await?;
        let fence = crate::fragment_index_cluster::open_source_fence(
            file,
            Some(&current_source.local.object_version()),
        )
        .await?;
        if loss.is_cancelled() || !fence.unchanged() {
            return Err("source changed before publication".to_owned());
        }
        if !replacement_exists {
            return Err("verified replacement is missing".to_owned());
        }
        // A successful probe only proves page-cache bytes. Persist the exact
        // replacement before the first operation that moves the source.
        let scratch_directory = context.scratch.directory.clone();
        let durable_replacement = context
            .bind_replacement(
                &scratch_directory,
                "replacement.mkv",
                loss,
                "verified replacement before staging",
            )
            .await?;
        durable_replacement
            .file
            .sync_all()
            .await
            .map_err(|error| format!("syncing replacement before staging: {error}"))?;
        require_held_unchanged(&durable_replacement, "replacement before staging").await?;
        if !context.keep_original {
            let guard_local = ensure_public_replacement_proof(
                &scratch_directory,
                "replacement.mkv",
                &durable_replacement.local,
                &scratch_directory,
            )
            .await?;
            context.rebaseline_replacement(guard_local).await?;
        }
        if loss.is_cancelled() || !fence.unchanged() {
            return Err("source changed while the replacement was made durable".to_owned());
        }
        let Some(staged_local) = rename_expected_noreplace_between(
            &context.source_parent,
            &context.source_name,
            &current_source.local,
            &context.scratch.directory,
            "source.p7.original",
        )
        .await?
        else {
            return Err("refusing to overwrite an existing staged original".to_owned());
        };
        context.rebaseline_source(staged_local).await?;
        let scratch_directory = context.scratch.directory.clone();
        context
            .bind_source(
                &scratch_directory,
                "source.p7.original",
                loss,
                "staged source",
            )
            .await?;
    } else if !source_exists && !staged_exists {
        return Err("source and staged original are both missing".to_owned());
    }

    if !child_exists(&context.source_parent, &context.source_name).await? {
        if !child_exists(&context.scratch.directory, "replacement.mkv").await? {
            // Leave the staged inode recoverable. Restoring it is itself a
            // publication mutation and must only happen under a live lease.
            return Err("verified replacement disappeared before publication".to_owned());
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before replacement rename".to_owned());
        }
        let scratch_directory = context.scratch.directory.clone();
        let replacement = context
            .bind_replacement(
                &scratch_directory,
                "replacement.mkv",
                loss,
                "replacement before publication",
            )
            .await?;
        replacement
            .file
            .sync_all()
            .await
            .map_err(|error| format!("syncing replacement before publication: {error}"))?;
        require_held_unchanged(&replacement, "replacement before publication").await?;
        if !context.keep_original {
            let guard_local = ensure_public_replacement_proof(
                &scratch_directory,
                "replacement.mkv",
                &replacement.local,
                &scratch_directory,
            )
            .await?;
            context.rebaseline_replacement(guard_local).await?;
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before replacement rename".to_owned());
        }
        let Some(public_local) = rename_expected_noreplace_between(
            &context.scratch.directory,
            "replacement.mkv",
            &replacement.local,
            &context.source_parent,
            &context.source_name,
        )
        .await?
        else {
            return Err("a newer source appeared before replacement publication".to_owned());
        };
        context.rebaseline_replacement(public_local).await?;
    }

    let (probe, durable_public) = match published_probe_and_metadata(file, &mut context, loss).await
    {
        Ok(published) => published,
        Err(error) => {
            return rollback_published(file, &mut context, loss, error).await;
        }
    };
    durable_public
        .file
        .sync_all()
        .await
        .map_err(|error| format!("syncing published replacement: {error}"))?;
    require_held_unchanged(&durable_public, "published replacement after sync").await?;
    let size = i64::try_from(durable_public.content.size)
        .map_err(|_| "replacement is too large".to_owned())?;
    if size != context.expected_bytes {
        let expected = context.expected_bytes;
        return rollback_published(
            file,
            &mut context,
            loss,
            format!("published replacement is {size} bytes, expected {expected}"),
        )
        .await;
    }
    let mtime = durable_public.local.modified_seconds;
    let original_path = if context.keep_original {
        match retain_original_before_commit(&paths, &mut context, loss).await {
            Ok(path) => path,
            Err(error) if loss.is_cancelled() => return Err(error),
            Err(error) => return rollback_published(file, &mut context, loss, error).await,
        }
    } else {
        None
    };
    prune_published_scratch(&context, loss).await?;
    let recovery_guard = recovery_guard_intent(file).await?;
    Ok(PublicationOutcome::Published(Box::new(
        PublishedReplacement {
            original_path,
            recovery_guard_id: recovery_guard.as_ref().map(|guard| guard.guard_id.clone()),
            recovery_guard_path: recovery_guard.map(|guard| guard.recovery_path),
            bytes_after: size,
            probe,
            size,
            mtime,
        },
    )))
}

pub async fn recover_published(
    file: &MediaFile,
    node_id: &str,
    expected_bytes: i64,
    loss: &CancellationToken,
) -> Result<PublicationOutcome, String> {
    let paths = ConversionPaths::for_file(file)?;
    let mut context = publication_context(&paths, file, node_id, expected_bytes).await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before published recovery".to_owned());
    }
    if context.scratch.owner.rollback_reason.is_some() {
        let reason = context
            .scratch
            .owner
            .rollback_reason
            .clone()
            .expect("checked above");
        return rollback_published(file, &mut context, loss, reason).await;
    }
    if context.scratch.owner.discard_pending
        && !child_exists(&context.source_parent, &context.source_name).await?
        && child_exists(&context.scratch.directory, PUBLIC_PROOF_FILE).await?
    {
        let scratch_directory = context.scratch.directory.clone();
        let guard = context
            .bind_replacement(
                &scratch_directory,
                PUBLIC_PROOF_FILE,
                loss,
                "recovery guard for missing public replacement",
            )
            .await?;
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before restoring the public replacement".to_owned(),
            );
        }
        let restored = restore_public_from_recovery_guard(
            &scratch_directory,
            &guard.local,
            &context.source_parent,
            &context.source_name,
        )
        .await?;
        context.rebaseline_replacement(restored).await?;
    }
    let (public_before_probe, public_is_source, public_rebound) = bind_one_of(
        &context.source_parent,
        &context.source_name,
        &context.scratch.owner.source,
        &context.replacement,
        node_id,
        loss,
        "public child before published recovery",
    )
    .await?;
    if public_rebound {
        if public_is_source {
            context
                .scratch
                .owner
                .source
                .rebaseline(node_id, public_before_probe.local.clone());
        } else {
            context
                .replacement
                .rebaseline(node_id, public_before_probe.local.clone());
            context.scratch.owner.replacement = Some(context.replacement.clone());
        }
        write_scratch_owner(&context.scratch.directory, &context.scratch.owner).await?;
    }
    if public_is_source {
        // This is the normal verified-but-not-yet-published state. It must
        // fall through to verify_existing/publish_verified in state.rs, not
        // be interpreted as a failed post-publication replacement.
        return Err("public pathname is not the manifest-bound published replacement".to_owned());
    }
    let (probe, public) = match published_probe_and_metadata(file, &mut context, loss).await {
        Ok(published) => published,
        Err(error) => {
            return rollback_published(file, &mut context, loss, error).await;
        }
    };
    let size =
        i64::try_from(public.content.size).map_err(|_| "replacement is too large".to_owned())?;
    if size != expected_bytes {
        let error = format!(
            "published replacement size is {size}, verified ledger recorded {expected_bytes}"
        );
        return rollback_published(file, &mut context, loss, error).await;
    }
    let original = if child_exists(&context.scratch.directory, "source.p7.original").await? {
        let directory = context.scratch.directory.clone();
        Some(
            context
                .bind_source(
                    &directory,
                    "source.p7.original",
                    loss,
                    "staged recovery original",
                )
                .await?,
        )
    } else if context.keep_original
        && child_exists(&context.source_parent, &context.retained_name).await?
    {
        let directory = context.source_parent.clone();
        let name = context.retained_name.clone();
        Some(
            context
                .bind_source(&directory, &name, loss, "retained recovery original")
                .await?,
        )
    } else {
        None
    };
    if let Some(original) = original.as_ref() {
        let source_probe = probe_bound(
            original,
            &file.path,
            "original for published recovery",
            loss,
        )
        .await?;
        if let Err(error) = verify_replacement(&source_probe, &probe) {
            return rollback_published(file, &mut context, loss, error).await;
        }
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before recovery finalization".to_owned());
    }
    public
        .file
        .sync_all()
        .await
        .map_err(|error| format!("syncing published replacement during recovery: {error}"))?;
    require_held_unchanged(&public, "published replacement during recovery").await?;
    let original_path = if context.keep_original {
        retain_original_before_commit(&paths, &mut context, loss).await?
    } else {
        None
    };
    prune_published_scratch(&context, loss).await?;
    let recovery_guard = recovery_guard_intent(file).await?;
    Ok(PublicationOutcome::Published(Box::new(
        PublishedReplacement {
            original_path,
            recovery_guard_id: recovery_guard.as_ref().map(|guard| guard.guard_id.clone()),
            recovery_guard_path: recovery_guard.map(|guard| guard.recovery_path),
            bytes_after: size,
            probe,
            size,
            mtime: public.local.modified_seconds,
        },
    )))
}

pub async fn cleanup_after_failure(file: &MediaFile, loss: &CancellationToken) {
    let Ok(paths) = ConversionPaths::for_file(file) else {
        return;
    };
    if let Err(error) = reconcile_cleanup_quarantine(&paths, file, loss).await {
        tracing::warn!(%error, "reconciling failed Dolby Vision cleanup quarantine");
        return;
    }
    if !path_entry_exists(&paths.directory).await.unwrap_or(true) {
        return;
    }
    let scratch = match require_owned_scratch(&paths, file, None).await {
        Ok(scratch) => scratch,
        Err(error) => {
            tracing::warn!(%error, "refusing to clean unowned Dolby Vision scratch");
            return;
        }
    };
    match scratch.directory.child_metadata("source.p7.original").await {
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
    if child_exists(&scratch.directory, DISCARD_PENDING_FILE)
        .await
        .unwrap_or(true)
    {
        // This may be the only original after an interrupted verified
        // finalization. Only the verified recovery path may interpret it.
        return;
    }
    if let Err(error) = remove_owned_scratch(&paths, file, None, loss).await {
        tracing::warn!(%error, "cleaning failed Dolby Vision conversion scratch");
    }
}

pub async fn cleanup_after_commit(file: &MediaFile, node_id: &str, loss: &CancellationToken) {
    let Ok(paths) = ConversionPaths::for_file(file) else {
        return;
    };
    if let Err(error) = reconcile_cleanup_quarantine(&paths, file, loss).await {
        tracing::warn!(%error, "reconciling committed Dolby Vision cleanup quarantine");
        return;
    }
    if !path_entry_exists(&paths.directory).await.unwrap_or(true) {
        return;
    }
    let scratch = match require_owned_scratch(&paths, file, None).await {
        Ok(scratch) => scratch,
        Err(error) => {
            tracing::warn!(%error, "refusing to clean unowned Dolby Vision scratch");
            return;
        }
    };
    if scratch.owner.keep_original == Some(false) {
        if !child_exists(&scratch.directory, PUBLIC_PROOF_FILE)
            .await
            .unwrap_or(false)
        {
            tracing::warn!("committed Dolby Vision guard ledger has no filesystem guard");
        }
        // The non-cascading recovery-guard ledger owns this scratch until a
        // confirmed file-row deletion drives its monotone orphan cleanup.
        return;
    }
    drop(scratch);
    let _ = node_id;
    if let Err(error) = remove_owned_scratch(&paths, file, None, loss).await {
        tracing::warn!(%error, "cleaning committed Dolby Vision conversion scratch");
    }
}

fn recovery_guard_witness_name(file_id: i64, source_path: &str, guard_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_id.to_be_bytes());
    for value in [guard_id.as_bytes(), source_path.as_bytes()] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    format!(".plurx-dv-recovery-guard-{:x}.json", hasher.finalize())
}

fn expected_recovery_guard_witness(
    file_id: i64,
    source_path: &str,
    guard_id: &str,
    state: RecoveryGuardWitnessState,
) -> RecoveryGuardWitness {
    RecoveryGuardWitness {
        version: RECOVERY_GUARD_WITNESS_VERSION,
        guard_id: guard_id.to_owned(),
        file_id,
        source_path: source_path.to_owned(),
        state,
    }
}

async fn read_recovery_guard_witness(
    parent: &SecureDirectory,
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<Option<RecoveryGuardWitness>, String> {
    let name = recovery_guard_witness_name(file_id, source_path, guard_id);
    let metadata = match parent.child_metadata(&name).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspecting recovery-guard filesystem witness: {error}"
            ));
        }
    };
    let parent_identity = parent
        .identity()
        .await
        .map_err(|error| format!("identifying recovery-guard source parent: {error}"))?;
    if !metadata.is_file || metadata.identity.device != parent_identity.device {
        return Err(
            "recovery-guard witness is not a regular child on the source filesystem".to_owned(),
        );
    }
    let bytes = parent
        .read_bounded_child(&name, MAX_RECOVERY_GUARD_WITNESS_BYTES)
        .await
        .map_err(|error| format!("reading recovery-guard filesystem witness: {error}"))?;
    let witness: RecoveryGuardWitness = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decoding recovery-guard filesystem witness: {error}"))?;
    if witness.version != RECOVERY_GUARD_WITNESS_VERSION
        || witness.guard_id != guard_id
        || witness.file_id != file_id
        || witness.source_path != source_path
    {
        return Err(
            "recovery-guard filesystem witness does not match its ledger identity".to_owned(),
        );
    }
    Ok(Some(witness))
}

async fn write_recovery_guard_witness(
    parent: &SecureDirectory,
    witness: &RecoveryGuardWitness,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(witness)
        .map_err(|error| format!("encoding recovery-guard filesystem witness: {error}"))?;
    if bytes.len() as u64 > MAX_RECOVERY_GUARD_WITNESS_BYTES {
        return Err("recovery-guard filesystem witness exceeds its bounded format".to_owned());
    }
    let name =
        recovery_guard_witness_name(witness.file_id, &witness.source_path, &witness.guard_id);
    parent
        .atomic_write_child(&name, &bytes)
        .await
        .map_err(|error| format!("persisting recovery-guard filesystem witness: {error}"))?;
    let durable = read_recovery_guard_witness(
        parent,
        witness.file_id,
        &witness.source_path,
        &witness.guard_id,
    )
    .await?
    .ok_or_else(|| "durable recovery-guard witness disappeared after fsync".to_owned())?;
    if durable != *witness {
        return Err("durable recovery-guard witness changed after fsync".to_owned());
    }
    Ok(())
}

async fn ensure_guard_attested_witness(
    parent: &SecureDirectory,
    scratch: &SecureDirectory,
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<(), String> {
    let parent_identity = parent
        .identity()
        .await
        .map_err(|error| format!("identifying recovery-guard source parent: {error}"))?;
    let scratch_identity = scratch
        .identity()
        .await
        .map_err(|error| format!("identifying recovery-guard scratch: {error}"))?;
    if parent_identity.device != scratch_identity.device {
        return Err(
            "recovery scratch and source parent are not on the same media filesystem".to_owned(),
        );
    }
    match read_recovery_guard_witness(parent, file_id, source_path, guard_id).await? {
        Some(witness) if witness.state == RecoveryGuardWitnessState::GuardAttested => Ok(()),
        Some(_) => Err(
            "terminal recovery-guard tombstone appeared before guard cleanup completed".to_owned(),
        ),
        None => {
            let witness = expected_recovery_guard_witness(
                file_id,
                source_path,
                guard_id,
                RecoveryGuardWitnessState::GuardAttested,
            );
            write_recovery_guard_witness(parent, &witness).await
        }
    }
}

async fn require_guard_attested_witness(
    parent: &SecureDirectory,
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<(), String> {
    match read_recovery_guard_witness(parent, file_id, source_path, guard_id).await? {
        Some(witness) if witness.state == RecoveryGuardWitnessState::GuardAttested => Ok(()),
        Some(_) => Err("recovery-guard witness is already a terminal tombstone".to_owned()),
        None => Err(
            "recovery scratch is absent without a mounted-filesystem witness; refusing cleanup"
                .to_owned(),
        ),
    }
}

async fn retire_recovery_guard_witness(
    parent: &SecureDirectory,
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<(), String> {
    match read_recovery_guard_witness(parent, file_id, source_path, guard_id).await? {
        Some(witness) if witness.state == RecoveryGuardWitnessState::ScratchRemoved => Ok(()),
        Some(witness) if witness.state == RecoveryGuardWitnessState::GuardAttested => {
            let tombstone = RecoveryGuardWitness {
                state: RecoveryGuardWitnessState::ScratchRemoved,
                ..witness
            };
            write_recovery_guard_witness(parent, &tombstone).await
        }
        Some(_) => Err("recovery-guard witness has an invalid retirement state".to_owned()),
        None => Err(
            "recovery scratch is absent without a mounted-filesystem witness; refusing retirement"
                .to_owned(),
        ),
    }
}

pub(crate) async fn attest_orphan_recovery_tombstone(
    file_id: i64,
    source_path: &str,
    guard_id: &str,
) -> Result<RecoveryGuardTombstoneAttestation, String> {
    let source = Path::new(source_path);
    let parent_path = source
        .parent()
        .ok_or_else(|| "recovery-guard source has no parent directory".to_owned())?;
    let parent = SecureDirectory::open(parent_path)
        .await
        .map_err(|error| format!("opening recovery-guard tombstone parent safely: {error}"))?;
    match read_recovery_guard_witness(&parent, file_id, source_path, guard_id).await? {
        Some(witness) if witness.state == RecoveryGuardWitnessState::ScratchRemoved => {
            Ok(RecoveryGuardTombstoneAttestation {
                _source_parent: parent,
            })
        }
        Some(_) => Err("recovery-guard witness is not a terminal tombstone".to_owned()),
        None => Err(
            "terminal recovery witness is absent; refusing to delete the guard ledger row"
                .to_owned(),
        ),
    }
}

pub async fn remove_orphan_recovery_guard(
    file_id: i64,
    source_path: &str,
    recovery_path: &str,
    guard_id: &str,
    node_id: &str,
    loss: &CancellationToken,
) -> Result<(), String> {
    let source = Path::new(source_path);
    let paths = ConversionPaths::for_source(file_id, source)?;
    let parent_path = source
        .parent()
        .ok_or_else(|| "recovery-guard source has no parent directory".to_owned())?;
    let parent = SecureDirectory::open(parent_path)
        .await
        .map_err(|error| format!("opening orphan guard parent safely: {error}"))?;
    let expected_path = paths.directory.join(PUBLIC_PROOF_FILE);
    if Path::new(recovery_path) != expected_path {
        return Err("recovery-guard ledger path is not the deterministic owned child".to_owned());
    }
    if !child_exists(&parent, &paths.directory_name).await? {
        require_guard_attested_witness(&parent, file_id, source_path, guard_id).await?;
        return Ok(());
    }
    let scratch_directory = parent
        .open_child_directory(&paths.directory_name)
        .await
        .map_err(|error| format!("opening orphan guard scratch safely: {error}"))?;
    let interrupted_guard = delete_quarantine_name(PUBLIC_PROOF_FILE);
    let final_guard = final_delete_quarantine_name(PUBLIC_PROOF_FILE);
    if child_exists(&scratch_directory, &final_guard).await? {
        if child_exists(&scratch_directory, &interrupted_guard).await? {
            return Err("guard delete quarantine and one-shot removal child both exist".to_owned());
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before one-shot guard recovery".to_owned());
        }
        if !scratch_directory
            .rename_child_noreplace(&final_guard, &interrupted_guard)
            .await
            .map_err(|error| format!("restoring one-shot guard removal: {error}"))?
        {
            return Err("guard delete quarantine reappeared during recovery".to_owned());
        }
        sync_directory(&paths.directory).await?;
    }
    if !child_exists(&scratch_directory, PUBLIC_PROOF_FILE).await?
        && child_exists(&scratch_directory, &interrupted_guard).await?
    {
        if loss.is_cancelled() {
            return Err("conversion lease was lost before guard-delete recovery".to_owned());
        }
        if !scratch_directory
            .rename_child_noreplace(&interrupted_guard, PUBLIC_PROOF_FILE)
            .await
            .map_err(|error| format!("restoring interrupted guard deletion: {error}"))?
        {
            return Err("guard reappeared during interrupted deletion recovery".to_owned());
        }
        sync_directory(&paths.directory).await?;
    }
    let mut scratch =
        require_recovery_guard_scratch(&paths, file_id, source_path, guard_id).await?;
    let held_identity = scratch_directory
        .identity()
        .await
        .map_err(|error| format!("identifying held orphan guard scratch: {error}"))?;
    let scratch_identity = scratch
        .directory
        .identity()
        .await
        .map_err(|error| format!("identifying validated orphan guard scratch: {error}"))?;
    if !held_identity.same_inode(scratch_identity) {
        return Err("recovery-guard scratch changed while its ownership was validated".to_owned());
    }
    if !logical_child_exists(&scratch.directory, PUBLIC_PROOF_FILE).await? {
        let staged_original =
            logical_child_exists(&scratch.directory, "source.p7.original").await?;
        let discard_pending =
            logical_child_exists(&scratch.directory, DISCARD_PENDING_FILE).await?;
        if staged_original || discard_pending {
            return Err(
                "refusing an unmaterialized recovery guard while scratch retains an original"
                    .to_owned(),
            );
        }
        // The Store intent is durable before publication creates the hard-link
        // proof. A crash in that interval may later leave an orphaned intent,
        // but it has not moved or deleted the source. Once the owned scratch
        // proves that neither original-bearing namespace exists, attest that
        // proof removal is already complete and let the ordinary monotone
        // GuardRemoved -> ScratchRemoved lifecycle retire the scratch.
        ensure_guard_attested_witness(&parent, &scratch.directory, file_id, source_path, guard_id)
            .await?;
        return Ok(());
    }
    let expected = scratch
        .owner
        .replacement
        .clone()
        .ok_or_else(|| "recovery-guard scratch has no replacement identity".to_owned())?;
    let proof_visible = child_exists(&scratch.directory, PUBLIC_PROOF_FILE).await?;
    let proof_directory = if proof_visible {
        scratch.directory.clone()
    } else {
        SecureDirectory::open(&paths.directory.join(PRIVATE_DELETE_ANCHOR))
            .await
            .map_err(|error| format!("opening private recovery-guard delete anchor: {error}"))?
    };
    let proof_name = if proof_visible {
        PUBLIC_PROOF_FILE.to_owned()
    } else {
        private_delete_slot(PUBLIC_PROOF_FILE, PrivateDeleteKind::File)
    };
    let (guard, rebound) = bind_expected_content(
        &proof_directory,
        &proof_name,
        &expected,
        node_id,
        loss,
        "orphaned replacement recovery guard",
    )
    .await?;
    if rebound {
        scratch
            .owner
            .replacement
            .as_mut()
            .expect("checked above")
            .rebaseline(node_id, guard.local.clone());
        if loss.is_cancelled() {
            return Err("conversion lease was lost before guard rebaseline".to_owned());
        }
        write_scratch_owner(&scratch.directory, &scratch.owner).await?;
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before orphan guard removal".to_owned());
    }
    if guard.local.device != scratch_identity.device {
        return Err("recovery guard is not on its owned scratch filesystem".to_owned());
    }
    ensure_guard_attested_witness(&parent, &scratch.directory, file_id, source_path, guard_id)
        .await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost after recovery-guard attestation".to_owned());
    }
    unlink_expected_child(&scratch.directory, PUBLIC_PROOF_FILE, &guard.local).await
}

pub async fn remove_orphan_recovery_scratch(
    file_id: i64,
    source_path: &str,
    guard_id: &str,
    loss: &CancellationToken,
) -> Result<(), String> {
    let source = Path::new(source_path);
    let paths = ConversionPaths::for_source(file_id, source)?;
    let parent_path = source
        .parent()
        .ok_or_else(|| "recovery-guard source has no parent directory".to_owned())?;
    let parent = SecureDirectory::open(parent_path)
        .await
        .map_err(|error| format!("opening orphan guard parent safely: {error}"))?;
    let witness = read_recovery_guard_witness(&parent, file_id, source_path, guard_id)
        .await?
        .ok_or_else(|| {
            "recovery scratch has no mounted-filesystem witness; refusing cleanup".to_owned()
        })?;
    let final_cleanup_name = format!("{}.removing", paths.cleanup_name);
    if child_exists(&parent, &final_cleanup_name).await? {
        if child_exists(&parent, &paths.cleanup_name).await? {
            return Err("orphan cleanup root and one-shot removal root both exist".to_owned());
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before orphan cleanup-root recovery".to_owned());
        }
        if !parent
            .rename_child_noreplace(&final_cleanup_name, &paths.cleanup_name)
            .await
            .map_err(|error| format!("restoring one-shot orphan cleanup root: {error}"))?
        {
            return Err("orphan cleanup root reappeared during recovery".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    let active = child_exists(&parent, &paths.directory_name).await?;
    let cleanup = logical_directory_exists(&parent, &paths.cleanup_name).await?;
    if active && cleanup {
        return Err("active and cleanup recovery-guard scratch both exist".to_owned());
    }
    if !active && !cleanup {
        return retire_recovery_guard_witness(&parent, file_id, source_path, guard_id).await;
    }
    if witness.state == RecoveryGuardWitnessState::ScratchRemoved {
        return Err("terminal recovery tombstone conflicts with remaining scratch".to_owned());
    }
    let mut cleanup_paths = paths.clone();
    if active {
        let scratch =
            require_recovery_guard_scratch(&paths, file_id, source_path, guard_id).await?;
        if logical_child_exists(&scratch.directory, PUBLIC_PROOF_FILE).await? {
            return Err("refusing to remove recovery scratch before its guard".to_owned());
        }
        if logical_child_exists(&scratch.directory, "source.p7.original").await?
            || logical_child_exists(&scratch.directory, DISCARD_PENDING_FILE).await?
        {
            return Err("refusing orphan scratch that still contains an original".to_owned());
        }
        if loss.is_cancelled() {
            return Err("conversion lease was lost before orphan scratch quarantine".to_owned());
        }
        if !parent
            .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
            .await
            .map_err(|error| format!("quarantining orphan recovery scratch: {error}"))?
        {
            return Err("orphan recovery cleanup destination already exists".to_owned());
        }
        sync_directory(parent_path).await?;
    }
    let cleanup_visible = child_exists(&parent, &paths.cleanup_name).await?;
    cleanup_paths.directory = if cleanup_visible {
        parent_path.join(&paths.cleanup_name)
    } else {
        parent_path
            .join(PRIVATE_DELETE_ANCHOR)
            .join(private_delete_slot(
                &paths.cleanup_name,
                PrivateDeleteKind::Directory,
            ))
    };
    cleanup_paths.directory_name = paths.cleanup_name.clone();
    if cleanup_visible {
        restore_interrupted_scratch_owner(&cleanup_paths, loss).await?;
    } else {
        let anchored = SecureDirectory::open(&cleanup_paths.directory)
            .await
            .map_err(|error| format!("opening anchored orphan cleanup root: {error}"))?;
        let expected = anchored
            .identity()
            .await
            .map_err(|error| format!("identifying anchored orphan cleanup root: {error}"))?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before anchored orphan cleanup".to_owned());
        }
        remove_flat_tree_expected(&parent, &paths.cleanup_name, &anchored, expected, loss).await?;
        if loss.is_cancelled() {
            return Err("conversion lease was lost before recovery-witness retirement".to_owned());
        }
        return retire_recovery_guard_witness(&parent, file_id, source_path, guard_id).await;
    }
    let scratch =
        require_recovery_guard_scratch(&cleanup_paths, file_id, source_path, guard_id).await?;
    if logical_child_exists(&scratch.directory, PUBLIC_PROOF_FILE).await?
        || logical_child_exists(&scratch.directory, "source.p7.original").await?
        || logical_child_exists(&scratch.directory, DISCARD_PENDING_FILE).await?
    {
        return Err("refusing non-terminal orphan recovery scratch".to_owned());
    }
    let expected = scratch
        .directory
        .identity()
        .await
        .map_err(|error| format!("identifying orphan cleanup scratch: {error}"))?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before orphan scratch removal".to_owned());
    }
    remove_flat_tree_expected(
        &parent,
        &paths.cleanup_name,
        &scratch.directory,
        expected,
        loss,
    )
    .await?;
    if loss.is_cancelled() {
        return Err("conversion lease was lost before recovery-witness retirement".to_owned());
    }
    retire_recovery_guard_witness(&parent, file_id, source_path, guard_id).await
}

async fn restore_interrupted_scratch_owner(
    paths: &ConversionPaths,
    loss: &CancellationToken,
) -> Result<(), String> {
    let directory = SecureDirectory::open(&paths.directory)
        .await
        .map_err(|error| format!("opening interrupted cleanup scratch: {error}"))?;
    let interrupted = delete_quarantine_name(SCRATCH_OWNER_FILE);
    let final_interrupted = final_delete_quarantine_name(SCRATCH_OWNER_FILE);
    if child_exists(&directory, SCRATCH_OWNER_FILE).await? {
        return Ok(());
    }
    if private_delete_pending(&directory, SCRATCH_OWNER_FILE, PrivateDeleteKind::File).await? {
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before private owner-marker recovery".to_owned(),
            );
        }
        if restore_private_delete_slot(&directory, SCRATCH_OWNER_FILE, PrivateDeleteKind::File)
            .await?
        {
            return Ok(());
        }
    }
    if child_exists(&directory, &final_interrupted).await? {
        if child_exists(&directory, &interrupted).await? {
            return Err(
                "owner-marker delete quarantine and one-shot removal child both exist".to_owned(),
            );
        }
        if loss.is_cancelled() {
            return Err(
                "conversion lease was lost before owner-marker removal recovery".to_owned(),
            );
        }
        if !directory
            .rename_child_noreplace(&final_interrupted, &interrupted)
            .await
            .map_err(|error| format!("restoring one-shot owner-marker removal: {error}"))?
        {
            return Err("owner-marker delete quarantine reappeared during recovery".to_owned());
        }
        sync_directory(&paths.directory).await?;
    }
    if !child_exists(&directory, &interrupted).await? {
        return Err("orphan cleanup scratch has no owned marker".to_owned());
    }
    if loss.is_cancelled() {
        return Err("conversion lease was lost before owner-marker recovery".to_owned());
    }
    if !directory
        .rename_child_noreplace(&interrupted, SCRATCH_OWNER_FILE)
        .await
        .map_err(|error| format!("restoring interrupted owner-marker deletion: {error}"))?
    {
        return Err("scratch owner marker reappeared during recovery".to_owned());
    }
    sync_directory(&paths.directory).await
}

async fn run_tool(
    program: &str,
    args: &[OsString],
    loss: &CancellationToken,
) -> Result<String, String> {
    run_tool_with_timeout(program, args, loss, TOOL_RUN_TIMEOUT).await
}

async fn run_tool_with_timeout(
    program: &str,
    args: &[OsString],
    loss: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    run_tool_command_with_timeout(program, command, loss, timeout).await
}

#[cfg(unix)]
async fn run_tool_with_bound_media(
    program: &str,
    args: &[OsString],
    media_argument: usize,
    media: &BoundMedia,
    role: &str,
    loss: &CancellationToken,
) -> Result<String, String> {
    use std::os::unix::process::CommandExt;

    require_held_unchanged(media, role).await?;
    let duplicated = unsafe {
        libc::fcntl(
            media.file.as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            CHILD_MEDIA_FD_MIN,
        )
    };
    if duplicated < 0 {
        return Err(format!(
            "duplicating held descriptor for {role}: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: fcntl returned a fresh descriptor owned by this scope. It stays
    // alive until spawn has copied the command state into the child.
    let inherited = unsafe { OwnedFd::from_raw_fd(duplicated) };
    let inherited_fd = inherited.as_raw_fd();
    // F_DUPFD shares an open-file description. fdescfs on macOS may also
    // duplicate that description when the tool opens /dev/fd/N, so reset the
    // exact regular file before each independent consumer.
    if unsafe { libc::lseek(inherited_fd, 0, libc::SEEK_SET) } < 0 {
        return Err(format!(
            "rewinding held descriptor for {role}: {}",
            io::Error::last_os_error()
        ));
    }
    let mut child_args = args.to_vec();
    let media_argument = child_args
        .get_mut(media_argument)
        .ok_or_else(|| format!("{program} has no descriptor-bound media argument for {role}"))?;
    #[cfg(target_os = "linux")]
    let inherited_path = format!("/proc/self/fd/{inherited_fd}");
    #[cfg(not(target_os = "linux"))]
    let inherited_path = format!("/dev/fd/{inherited_fd}");
    *media_argument = inherited_path.into();

    let mut command = tokio::process::Command::new(program);
    command.args(&child_args);
    // SAFETY: fcntl is async-signal-safe. The duplicated descriptor is the
    // sole descriptor whose close-on-exec bit this command clears, so the
    // child receives the manifest-bound source without widening inheritance.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            let flags = libc::fcntl(inherited_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(inherited_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let result = run_tool_command_with_timeout(program, command, loss, TOOL_RUN_TIMEOUT).await;
    // Even a failed or cancelled tool is not allowed to hide a concurrent
    // mutation of the exact source inode it consumed.
    let unchanged = require_held_unchanged(media, role).await;
    drop(inherited);
    unchanged?;
    result
}

#[cfg(windows)]
async fn run_tool_with_bound_media(
    program: &str,
    args: &[OsString],
    media_argument: usize,
    media: &BoundMedia,
    role: &str,
    loss: &CancellationToken,
) -> Result<String, String> {
    require_held_unchanged(media, role).await?;
    let held = media
        .file
        .try_clone()
        .await
        .map_err(|error| format!("duplicating held Windows media for {role}: {error}"))?
        .into_std()
        .await;
    let path = plurx_core::fs_secure::std_file_path(&held)
        .map_err(|error| format!("resolving held Windows media for {role}: {error}"))?;
    let mut child_args = args.to_vec();
    *child_args
        .get_mut(media_argument)
        .ok_or_else(|| format!("{program} has no bound media argument for {role}"))? =
        path.into_os_string();
    let mut command = tokio::process::Command::new(program);
    command.args(child_args);
    let result = run_tool_command_with_timeout(program, command, loss, TOOL_RUN_TIMEOUT).await;
    require_held_unchanged(media, role).await?;
    result
}

#[cfg(not(any(unix, windows)))]
async fn run_tool_with_bound_media(
    _program: &str,
    _args: &[OsString],
    _media_argument: usize,
    _media: &BoundMedia,
    role: &str,
    _loss: &CancellationToken,
) -> Result<String, String> {
    Err(format!(
        "descriptor-bound conversion tools are unsupported for {role}"
    ))
}

async fn run_tool_command_with_timeout(
    program: &str,
    mut command: tokio::process::Command,
    loss: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("Dolby Vision conversion"),
    )
    .map_err(|error| format!("starting {program}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} stderr was not piped"))?;
    let execution = async move {
        tokio::try_join!(
            read_bounded_stream(stdout, MAX_TOOL_OUTPUT, "conversion tool stdout"),
            read_bounded_stream(stderr, MAX_TOOL_OUTPUT, "conversion tool stderr"),
            child.wait(),
        )
    };
    let (stdout, stderr, status) = tokio::select! {
        output = tokio::time::timeout(timeout, execution) => output
            .map_err(|_| format!("{program} exceeded its {timeout:?} execution deadline"))?
            .map_err(|error| format!("running {program}: {error}"))?,
        () = loss.cancelled() => return Err(format!("{program} cancelled after conversion lease loss")),
    };
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    if !status.success() {
        let detail = [stderr.as_str(), stdout.as_str()]
            .into_iter()
            .find(|text| !text.trim().is_empty())
            .unwrap_or("no diagnostic output")
            .trim();
        return Err(format!(
            "{program} exited with {}: {detail}",
            status
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
    if replacement.dolby_vision.bl_compat_id != Some(1) {
        return Err(format!(
            "replacement bl_compat_id is {:?}, expected 1 for Profile 8.1",
            replacement.dolby_vision.bl_compat_id
        ));
    }
    if replacement.dolby_vision.el_present != Some(false) {
        return Err(format!(
            "replacement el_present_flag is {:?}, expected 0",
            replacement.dolby_vision.el_present
        ));
    }
    if replacement.dolby_vision.rpu_present != Some(true) {
        return Err(format!(
            "replacement rpu_present_flag is {:?}, expected 1",
            replacement.dolby_vision.rpu_present
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
pub(crate) use tests::prepare_test_published_recovery_guard;

#[cfg(test)]
mod tests;
