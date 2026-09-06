//! Descriptor-bound decoder fact collection and its bounded node-local cache.
//!
//! The source handle is opened and authorized by the preparation owner. This
//! module never reopens its pathname: FFprobe receives a duplicate of that
//! exact handle, and metadata is checked again after probing before the facts
//! can be returned or cached.

use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::transcode::{DecodeCatalogMetadata, DecodeFacts, DecodeSourceIdentity};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const MAX_PROBE_STDOUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_STDERR_BYTES: usize = 16 * 1024;
const MAX_PROBE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64 * 1024;
const VERSION_DEADLINE: Duration = Duration::from_secs(5);
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    source: DecodeSourceIdentity,
    ffprobe_build_digest: String,
    catalog_digest: Option<String>,
    selected_stream: ProbeStreamSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // FirstPlayable is consumed by M2's selected-stream adapter.
pub(crate) enum ProbeStreamSelection {
    FirstPlayable,
    LegacyVideoOrdinal(u32),
    Absolute(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeFileIdentity {
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    device: u64,
    inode: u64,
    content_digest: String,
}

/// Startup-bound FFprobe identity. Fact collection executes the canonical
/// pathname and re-hashes it immediately before every cache lookup and after
/// every child exit, so a replaced binary can neither consume nor populate
/// facts under the old build identity.
#[derive(Debug, Clone)]
pub(crate) struct DecodeProbeIdentity {
    executable: PathBuf,
    build_digest: String,
    file: ProbeFileIdentity,
}

impl DecodeProbeIdentity {
    pub(crate) async fn discover(bin: &str) -> Result<Self, DecodeFactError> {
        let executable = resolve_executable(bin)?;
        let before_path = executable.clone();
        let before = tokio::task::spawn_blocking(move || probe_file_identity(&before_path))
            .await
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))??;
        let version = probe_version(&executable).await?;
        let after_path = executable.clone();
        let after = tokio::task::spawn_blocking(move || probe_file_identity(&after_path))
            .await
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))??;
        if before != after {
            return Err(DecodeFactError::ProbeChanged);
        }
        let digest_input = serde_json::json!({
            "canonical_path": &executable,
            "content_digest": &before.content_digest,
            "version": &version,
        });
        Ok(Self {
            executable,
            build_digest: hex::encode(Sha256::digest(digest_input.to_string().as_bytes())),
            file: before,
        })
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn build_digest(&self) -> &str {
        &self.build_digest
    }

    async fn validate_current(&self) -> Result<(), DecodeFactError> {
        let path = self.executable.clone();
        let current = tokio::task::spawn_blocking(move || probe_file_identity(&path))
            .await
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))??;
        if current == self.file {
            Ok(())
        } else {
            Err(DecodeFactError::ProbeChanged)
        }
    }
}

#[cfg(unix)]
fn resolve_executable(bin: &str) -> Result<PathBuf, DecodeFactError> {
    use std::os::unix::fs::PermissionsExt;

    let candidate = Path::new(bin);
    let resolved = if candidate.components().count() > 1 {
        candidate.to_owned()
    } else {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .map(|directory| directory.join(candidate))
            .find(|path| {
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable is not on PATH".to_owned()))?
    };
    std::fs::canonicalize(resolved)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))
}

#[cfg(not(unix))]
fn resolve_executable(_bin: &str) -> Result<PathBuf, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(unix)]
fn probe_file_identity(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;

    let mut file = std::fs::File::open(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PROBE_EXECUTABLE_BYTES {
        return Err(DecodeFactError::ProbeIdentity(
            "executable size is outside the bounded identity envelope".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut remaining = MAX_PROBE_EXECUTABLE_BYTES;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .by_ref()
            .take(remaining.saturating_add(1))
            .read(&mut buffer)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        remaining = remaining
            .checked_sub(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| {
                DecodeFactError::ProbeIdentity("executable exceeded identity limit".to_owned())
            })?;
        hasher.update(&buffer[..read]);
    }
    Ok(ProbeFileIdentity {
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        device: metadata.dev(),
        inode: metadata.ino(),
        content_digest: hex::encode(hasher.finalize()),
    })
}

#[cfg(not(unix))]
fn probe_file_identity(_path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

async fn probe_version(path: &Path) -> Result<Vec<u8>, DecodeFactError> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let outcome = tokio::time::timeout(VERSION_DEADLINE, async {
        let (stdout, status) = tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), child.wait());
        (stdout, status)
    })
    .await;
    let (stdout, status) = match outcome {
        Ok((stdout, status)) => (
            stdout?,
            status.map_err(|error| DecodeFactError::Read(error.to_string()))?,
        ),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(DecodeFactError::Deadline);
        }
    };
    if stdout.1 || stdout.0.is_empty() || !status.success() {
        return Err(DecodeFactError::ProbeIdentity(
            "bounded version probe failed".to_owned(),
        ));
    }
    Ok(stdout.0)
}

/// Bounded FIFO cache. The preparation path supplies the exact FFprobe build
/// digest, so replacing the binary cannot reuse facts from an older parser.
pub(crate) struct DecodeFactCache {
    entries: tokio::sync::Mutex<BTreeMap<CacheKey, DecodeFacts>>,
    order: tokio::sync::Mutex<VecDeque<CacheKey>>,
    /// One bounded global lane serializes executable revalidation and cache
    /// misses. Fact preparation cannot fan out unbounded hashing or child
    /// processes when several sessions start together.
    probe_gate: tokio::sync::Mutex<()>,
    capacity: usize,
}

impl DecodeFactCache {
    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_CACHE_ENTRIES)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: tokio::sync::Mutex::new(BTreeMap::new()),
            order: tokio::sync::Mutex::new(VecDeque::new()),
            probe_gate: tokio::sync::Mutex::new(()),
            capacity: capacity.clamp(1, MAX_CACHE_ENTRIES),
        }
    }

    pub(crate) async fn get_or_probe(
        &self,
        probe: &DecodeProbeIdentity,
        source: Arc<std::fs::File>,
        catalog: Option<&DecodeCatalogMetadata>,
        selected_stream: ProbeStreamSelection,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<DecodeFacts, DecodeFactError> {
        let started = std::time::Instant::now();
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let gate = tokio::select! {
            biased;
            _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
            gate = tokio::time::timeout(remaining, self.probe_gate.lock()) => {
                gate.map_err(|_| DecodeFactError::Deadline)?
            }
        };
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        validate_probe_within(probe, remaining, cancelled).await?;
        let source_identity = source_identity(&source)?;
        let key = CacheKey {
            source: source_identity.clone(),
            ffprobe_build_digest: probe.build_digest().to_owned(),
            catalog_digest: catalog.map(|metadata| metadata.digest().to_owned()),
            selected_stream,
        };
        if let Some(facts) = self.entries.lock().await.get(&key).cloned() {
            return Ok(facts);
        }
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let facts = collect(
            probe.executable(),
            &source,
            source_identity,
            catalog,
            selected_stream,
            remaining,
            cancelled,
        )
        .await?;
        let remaining = budget.min(PROBE_DEADLINE).saturating_sub(started.elapsed());
        validate_probe_within(probe, remaining, cancelled).await?;
        let mut entries = self.entries.lock().await;
        if let Some(existing) = entries.get(&key) {
            return Ok(existing.clone());
        }
        let mut order = self.order.lock().await;
        while entries.len() >= self.capacity {
            let Some(oldest) = order.pop_front() else {
                return Err(DecodeFactError::CacheInvariant);
            };
            entries.remove(&oldest);
        }
        entries.insert(key.clone(), facts.clone());
        order.push_back(key);
        drop(gate);
        Ok(facts)
    }
}

async fn validate_probe_within(
    probe: &DecodeProbeIdentity,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), DecodeFactError> {
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(budget, probe.validate_current()) => {
            result.map_err(|_| DecodeFactError::Deadline)?
        }
    }
}

async fn wait_for_cancellation(cancelled: Option<&tokio_util::sync::CancellationToken>) {
    match cancelled {
        Some(cancelled) => cancelled.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeFactError {
    ProbeIdentity(String),
    ProbeChanged,
    SourceMetadata(String),
    SourceChanged,
    Spawn(String),
    MissingPipe,
    Deadline,
    Cancelled,
    Read(String),
    OversizedOutput,
    Failed(Option<i32>, String),
    InvalidJson(String),
    InvalidFacts(String),
    CacheInvariant,
    #[cfg(not(unix))]
    UnsupportedPlatform,
}

impl std::fmt::Display for DecodeFactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProbeIdentity(error) => write!(formatter, "identifying FFprobe: {error}"),
            Self::ProbeChanged => formatter.write_str("FFprobe changed after startup identity"),
            Self::SourceMetadata(error) => {
                write!(formatter, "reading bound source metadata: {error}")
            }
            Self::SourceChanged => {
                formatter.write_str("bound source changed during decoder probing")
            }
            Self::Spawn(error) => write!(formatter, "starting bound FFprobe: {error}"),
            Self::MissingPipe => formatter.write_str("bound FFprobe pipe was unavailable"),
            Self::Deadline => formatter.write_str("bound FFprobe exceeded its deadline"),
            Self::Cancelled => formatter.write_str("bound FFprobe was cancelled"),
            Self::Read(error) => write!(formatter, "reading bound FFprobe output: {error}"),
            Self::OversizedOutput => formatter.write_str("bound FFprobe output exceeded its limit"),
            Self::Failed(code, reason) => {
                write!(formatter, "bound FFprobe failed ({code:?}): {reason}")
            }
            Self::InvalidJson(error) => write!(formatter, "parsing bound FFprobe JSON: {error}"),
            Self::InvalidFacts(error) => {
                write!(formatter, "validating bound decode facts: {error}")
            }
            Self::CacheInvariant => {
                formatter.write_str("decoder fact cache ordering is inconsistent")
            }
            #[cfg(not(unix))]
            Self::UnsupportedPlatform => formatter
                .write_str("descriptor-bound decoder probing is unsupported on this platform"),
        }
    }
}

impl std::error::Error for DecodeFactError {}

#[cfg(unix)]
fn source_identity(source: &std::fs::File) -> Result<DecodeSourceIdentity, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = source
        .metadata()
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DecodeFactError::SourceMetadata(
            "source handle is not a regular file".to_owned(),
        ));
    }
    let body = serde_json::json!({
        "bytes": metadata.len(),
        "modified_seconds": metadata.mtime(),
        "modified_nanoseconds": metadata.mtime_nsec(),
        "changed_seconds": metadata.ctime(),
        "changed_nanoseconds": metadata.ctime_nsec(),
        "device": metadata.dev(),
        "inode": metadata.ino(),
    });
    DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(body.to_string().as_bytes())))
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))
}

#[cfg(not(unix))]
fn source_identity(_source: &std::fs::File) -> Result<DecodeSourceIdentity, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), DecodeFactError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut scratch = [0_u8; 8 * 1024];
    let mut overflow = false;
    loop {
        let count = reader
            .read(&mut scratch)
            .await
            .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&scratch[..count.min(remaining)]);
        overflow |= count > remaining;
    }
    Ok((retained, overflow))
}

#[cfg(unix)]
async fn collect(
    ffprobe: &Path,
    source: &std::fs::File,
    before: DecodeSourceIdentity,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<DecodeFacts, DecodeFactError> {
    use std::os::fd::AsRawFd;

    let source_fd = source.as_raw_fd();
    let source_offset = unsafe { libc::lseek(source_fd, 0, libc::SEEK_CUR) };
    if source_offset == -1 {
        return Err(DecodeFactError::SourceMetadata(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // A duplicated descriptor shares its open-file offset. FFprobe is allowed
    // to seek while inspecting the held inode, but the later FFmpeg spawn must
    // inherit the same offset the preparation owner held before probing.
    struct RestoreOffset {
        fd: std::os::fd::RawFd,
        offset: libc::off_t,
    }
    impl Drop for RestoreOffset {
        fn drop(&mut self) {
            unsafe {
                libc::lseek(self.fd, self.offset, libc::SEEK_SET);
            }
        }
    }
    let _restore_offset = RestoreOffset {
        fd: source_fd,
        offset: source_offset,
    };
    let mut command = tokio::process::Command::new(ffprobe);
    command.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_entries",
        "stream=index,codec_type,codec_name,profile,pix_fmt,width,height,bits_per_raw_sample,avg_frame_rate,r_frame_rate,color_range,color_space,color_transfer,color_primaries:stream_disposition=attached_pic:stream_side_data=side_data_type",
        "-show_streams",
        "/dev/fd/3",
    ]);
    unsafe {
        command.pre_exec(move || {
            let duplicate = libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 10);
            if duplicate == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(duplicate, 3) == -1 {
                libc::close(duplicate);
                return Err(std::io::Error::last_os_error());
            }
            libc::close(duplicate);
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let stderr = child.stderr.take().ok_or(DecodeFactError::MissingPipe)?;
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(DecodeFactError::Cancelled);
        }
        outcome = tokio::time::timeout(budget.min(PROBE_DEADLINE), async {
            let (stdout, stderr, status) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            );
            (stdout, stderr, status)
        }) => outcome,
    };
    let (stdout, stderr, status) = match outcome {
        Ok((stdout, stderr, status)) => (
            stdout?,
            stderr?,
            status.map_err(|error| DecodeFactError::Read(error.to_string()))?,
        ),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(DecodeFactError::Deadline);
        }
    };
    if stdout.1 || stderr.1 {
        return Err(DecodeFactError::OversizedOutput);
    }
    if !status.success() {
        let reason = String::from_utf8_lossy(&stderr.0)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("FFprobe gave no reason")
            .chars()
            .take(512)
            .collect();
        return Err(DecodeFactError::Failed(status.code(), reason));
    }
    if source_identity(source)? != before {
        return Err(DecodeFactError::SourceChanged);
    }
    let json: serde_json::Value = serde_json::from_slice(&stdout.0)
        .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?;
    match selected_stream {
        ProbeStreamSelection::FirstPlayable => match catalog {
            Some(catalog) => DecodeFacts::from_ffprobe_json_with_catalog(&json, before, catalog),
            None => DecodeFacts::from_ffprobe_json(&json, before),
        },
        ProbeStreamSelection::LegacyVideoOrdinal(ordinal) => {
            let index = absolute_video_ordinal(&json, ordinal).ok_or_else(|| {
                DecodeFactError::InvalidFacts("legacy video stream is missing".to_owned())
            })?;
            if catalog.is_some() {
                return Err(DecodeFactError::InvalidFacts(
                    "catalog metadata cannot be attached to a legacy ordinal".to_owned(),
                ));
            }
            DecodeFacts::from_ffprobe_json_at(&json, before, index)
        }
        ProbeStreamSelection::Absolute(index) => {
            if catalog.is_some() {
                return Err(DecodeFactError::InvalidFacts(
                    "catalog metadata cannot be attached to an explicit stream".to_owned(),
                ));
            }
            DecodeFacts::from_ffprobe_json_at(&json, before, index)
        }
    }
    .map_err(|error| DecodeFactError::InvalidFacts(error.to_string()))
}

fn absolute_video_ordinal(json: &serde_json::Value, ordinal: u32) -> Option<u32> {
    json.get("streams")?
        .as_array()?
        .iter()
        .filter(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })
        .nth(usize::try_from(ordinal).ok()?)?
        .get("index")?
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
}

#[cfg(not(unix))]
async fn collect(
    _ffprobe: &Path,
    _source: &std::fs::File,
    _before: DecodeSourceIdentity,
    _catalog: Option<&DecodeCatalogMetadata>,
    _selected_stream: ProbeStreamSelection,
    _budget: Duration,
    _cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<DecodeFacts, DecodeFactError> {
    Err(DecodeFactError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn executable(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).expect("write probe");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("make probe executable");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn facts_are_collected_from_the_held_descriptor_and_cached_by_build() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"bound source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open media"));
        std::fs::remove_file(&media).expect("unlink path after opening");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then
  printf '%s\n' 'ffprobe version test-build'
  exit 0
fi
test ! -e "$0.used" || exit 93
touch "$0.used"
test "$8" = "/dev/fd/3" || exit 91
test "$(cat "$8")" = "bound source" || exit 92
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001","disposition":{"attached_pic":0}}]}'
"###,
        );
        let cache = DecodeFactCache::with_capacity(2);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("probe identity");
        let first = cache
            .get_or_probe(
                &identity,
                Arc::clone(&source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("bound facts");
        assert_eq!(first.input_video_stream(), 4);
        let mut offset_view = source.try_clone().expect("clone source descriptor");
        assert_eq!(
            std::io::Seek::stream_position(&mut offset_view).expect("source offset"),
            0,
            "fact collection must restore the held descriptor's file offset"
        );
        let cached = cache
            .get_or_probe(
                &identity,
                source,
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("cache hit does not respawn");
        assert_eq!(cached.facts_digest(), first.facts_digest());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_probe_identity_is_refused_before_fact_collection() {
        let error = DecodeProbeIdentity::discover("plurx-no-such-ffprobe")
            .await
            .expect_err("missing probe");
        assert!(matches!(error, DecodeFactError::ProbeIdentity(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_probe_cannot_reuse_cached_facts() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open"));
        let probe = root.path().join("ffprobe-test");
        let body = r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###;
        executable(&probe, body);
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = DecodeFactCache::new();
        cache
            .get_or_probe(
                &identity,
                Arc::clone(&source),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("initial facts");
        executable(&probe, &body.replace("version one", "version two"));
        assert_eq!(
            cache
                .get_or_probe(
                    &identity,
                    source,
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_secs(2),
                    None,
                )
                .await,
            Err(DecodeFactError::ProbeChanged)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_same_key_misses_spawn_one_probe() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let source = Arc::new(std::fs::File::open(&media).expect("open"));
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
test ! -e "$0.used" || exit 93
touch "$0.used"
sleep 1
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = Arc::new(DecodeFactCache::new());
        let first_cache = Arc::clone(&cache);
        let first_source = Arc::clone(&source);
        let first_identity = identity.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe(
                    &first_identity,
                    first_source,
                    None,
                    ProbeStreamSelection::Absolute(4),
                    Duration::from_secs(3),
                    None,
                )
                .await
        });
        let second = cache.get_or_probe(
            &identity,
            source,
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(3),
            None,
        );
        let (first, second) = tokio::join!(first, second);
        let first = first.expect("join first").expect("first facts");
        let second = second.expect("second facts");
        assert_eq!(first.facts_digest(), second.facts_digest());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_while_waiting_for_probe_admission_is_bounded() {
        let root = crate::test_tempdir().expect("tempdir");
        let first_media = root.path().join("first.bin");
        let second_media = root.path().join("second.bin");
        std::fs::write(&first_media, b"first").expect("first media");
        std::fs::write(&second_media, b"second").expect("second media");
        let probe = root.path().join("ffprobe-test");
        executable(
            &probe,
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
touch "$0.started"
sleep 1
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
        );
        let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .expect("identity");
        let cache = Arc::new(DecodeFactCache::new());
        let first_cache = Arc::clone(&cache);
        let first_identity = identity.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe(
                    &first_identity,
                    Arc::new(std::fs::File::open(first_media).expect("first source")),
                    None,
                    ProbeStreamSelection::FirstPlayable,
                    Duration::from_secs(3),
                    None,
                )
                .await
        });
        for _ in 0..100 {
            if probe.with_extension("started").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            probe.with_extension("started").exists(),
            "first probe started"
        );
        let cancellation = tokio_util::sync::CancellationToken::new();
        let waiter = cache.get_or_probe(
            &identity,
            Arc::new(std::fs::File::open(second_media).expect("second source")),
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(3),
            Some(&cancellation),
        );
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("waiter returned before cancellation: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
        cancellation.cancel();
        assert_eq!(waiter.await, Err(DecodeFactError::Cancelled));
        first.await.expect("join first").expect("first facts");
    }

    #[tokio::test]
    async fn bounded_reader_drains_but_reports_oversize() {
        let bytes = vec![b'x'; 65];
        let (retained, overflow) = read_bounded(bytes.as_slice(), 64).await.expect("read");
        assert_eq!(retained.len(), 64);
        assert!(overflow);
    }

    #[test]
    fn legacy_video_ordinals_preserve_the_command_builders_exact_mapping() {
        let json = serde_json::json!({"streams": [
            {"index": 2, "codec_type": "audio"},
            {"index": 4, "codec_type": "video", "disposition": {"attached_pic": 1}},
            {"index": 7, "codec_type": "video", "disposition": {"attached_pic": 0}}
        ]});
        assert_eq!(absolute_video_ordinal(&json, 0), Some(4));
        assert_eq!(absolute_video_ordinal(&json, 1), Some(7));
        assert_eq!(absolute_video_ordinal(&json, 2), None);
    }
}
