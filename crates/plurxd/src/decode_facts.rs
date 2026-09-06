//! Descriptor-bound decoder fact collection and its bounded node-local cache.
//!
//! The source handle is opened and authorized by the preparation owner. This
//! module never reopens its pathname: FFprobe receives a duplicate of that
//! exact handle, and metadata is checked again after probing before the facts
//! can be returned or cached.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::transcode::{DecodeFacts, DecodeSourceIdentity};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const MAX_PROBE_STDOUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_STDERR_BYTES: usize = 16 * 1024;
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    source: DecodeSourceIdentity,
    ffprobe_build_digest: String,
    selected_stream: Option<u32>,
}

/// Bounded FIFO cache. The preparation path supplies the exact FFprobe build
/// digest, so replacing the binary cannot reuse facts from an older parser.
pub(crate) struct DecodeFactCache {
    entries: tokio::sync::Mutex<BTreeMap<CacheKey, DecodeFacts>>,
    order: tokio::sync::Mutex<VecDeque<CacheKey>>,
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
            capacity: capacity.clamp(1, MAX_CACHE_ENTRIES),
        }
    }

    pub(crate) async fn get_or_probe(
        &self,
        ffprobe: &str,
        ffprobe_build_digest: &str,
        source: Arc<std::fs::File>,
        selected_stream: Option<u32>,
    ) -> Result<DecodeFacts, DecodeFactError> {
        validate_digest(ffprobe_build_digest)?;
        let source_identity = source_identity(&source)?;
        let key = CacheKey {
            source: source_identity.clone(),
            ffprobe_build_digest: ffprobe_build_digest.to_owned(),
            selected_stream,
        };
        if let Some(facts) = self.entries.lock().await.get(&key).cloned() {
            return Ok(facts);
        }

        let facts = collect(ffprobe, &source, source_identity, selected_stream).await?;
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
        Ok(facts)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeFactError {
    InvalidBuildDigest,
    SourceMetadata(String),
    SourceChanged,
    Spawn(String),
    MissingPipe,
    Deadline,
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
            Self::InvalidBuildDigest => formatter.write_str("FFprobe build digest is invalid"),
            Self::SourceMetadata(error) => {
                write!(formatter, "reading bound source metadata: {error}")
            }
            Self::SourceChanged => {
                formatter.write_str("bound source changed during decoder probing")
            }
            Self::Spawn(error) => write!(formatter, "starting bound FFprobe: {error}"),
            Self::MissingPipe => formatter.write_str("bound FFprobe pipe was unavailable"),
            Self::Deadline => formatter.write_str("bound FFprobe exceeded its deadline"),
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

fn validate_digest(value: &str) -> Result<(), DecodeFactError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(DecodeFactError::InvalidBuildDigest)
    }
}

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
    ffprobe: &str,
    source: &std::fs::File,
    before: DecodeSourceIdentity,
    selected_stream: Option<u32>,
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
        "stream=index,codec_type,codec_name,profile,pix_fmt,width,height,bits_per_raw_sample,avg_frame_rate,r_frame_rate,color_transfer:stream_disposition=attached_pic:stream_side_data",
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
    let outcome = tokio::time::timeout(PROBE_DEADLINE, async {
        let (stdout, stderr, status) = tokio::join!(
            read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
            read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
            child.wait(),
        );
        (stdout, stderr, status)
    })
    .await;
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
        Some(index) => DecodeFacts::from_ffprobe_json_at(&json, before, index),
        None => DecodeFacts::from_ffprobe_json(&json, before),
    }
    .map_err(|error| DecodeFactError::InvalidFacts(error.to_string()))
}

#[cfg(not(unix))]
async fn collect(
    _ffprobe: &str,
    _source: &std::fs::File,
    _before: DecodeSourceIdentity,
    _selected_stream: Option<u32>,
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
test "$8" = "/dev/fd/3" || exit 91
test "$(cat "$8")" = "bound source" || exit 92
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001","disposition":{"attached_pic":0}}]}'
"###,
        );
        let cache = DecodeFactCache::with_capacity(2);
        let build = "a".repeat(64);
        let first = cache
            .get_or_probe(
                probe.to_str().expect("probe path"),
                &build,
                Arc::clone(&source),
                Some(4),
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
        std::fs::remove_file(&probe).expect("remove executable to prove cache hit");
        let cached = cache
            .get_or_probe("missing-probe", &build, source, Some(4))
            .await
            .expect("cache hit does not respawn");
        assert_eq!(cached.facts_digest(), first.facts_digest());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_build_identity_is_refused_before_spawn() {
        let root = crate::test_tempdir().expect("tempdir");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"source").expect("media");
        let error = DecodeFactCache::new()
            .get_or_probe(
                "missing-probe",
                "not-a-digest",
                Arc::new(std::fs::File::open(media).expect("open")),
                None,
            )
            .await
            .expect_err("invalid build digest");
        assert_eq!(error, DecodeFactError::InvalidBuildDigest);
    }

    #[tokio::test]
    async fn bounded_reader_drains_but_reports_oversize() {
        let bytes = vec![b'x'; 65];
        let (retained, overflow) = read_bounded(bytes.as_slice(), 64).await.expect("read");
        assert_eq!(retained.len(), 64);
        assert!(overflow);
    }
}
