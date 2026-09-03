//! Content-addressed fragment-index blobs, source attestation, and peer hydration.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::MediaFile;
use plurx_core::segplan::FragmentIndex;
use plurx_core::store::{
    cluster_fragment_index_blob_sha256, cluster_fragment_index_pipeline_digest,
    decode_cluster_fragment_index_blob, ClusterFragmentIndexArtifact, ClusterFragmentIndexLocation,
    FragmentIndexSourceObservation, Store, MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::http::peer_transport::{PeerAuthMode, PeerTransport};

pub(crate) const PEER_PATH_PREFIX: &str = "/internal/media/fragment-index/";
const PEER_DEADLINE: Duration = Duration::from_secs(8);
const HASH_CHUNK: usize = 256 * 1024;

/// One sampled extent. A megabyte is long enough that the seek in front of it
/// is amortised on a NAS rather than dominating the read.
pub(crate) const SAMPLE_EXTENT_BYTES: u64 = 1 << 20;
/// How many extents a sampled digest covers: head, tail, and 62 interior.
pub(crate) const SAMPLE_EXTENTS: u64 = 64;
/// At or below this size the whole file is read, because sampling a file
/// smaller than the sample buys nothing.
pub(crate) const SAMPLE_WHOLE_FILE_LIMIT: u64 = SAMPLE_EXTENTS * SAMPLE_EXTENT_BYTES;
/// Interior offsets land on filesystem block boundaries; NFS and SMB both
/// prefer it. The tail is exact and unaligned on purpose.
const SAMPLE_ALIGN: u64 = 4096;
/// Domain separation. A sampled digest can never equal a whole-file SHA-256
/// of the same bytes, nor a sampled digest taken under a different layout.
/// Changing the layout means changing this token and re-attesting.
const SAMPLE_DOMAIN: &[u8] = b"plurx/source-attestation/sampled-v1\0";

/// The byte ranges a sampled digest covers, ascending, as `(offset, len)`.
///
/// The head extent is at zero and the tail extent ends exactly at `size`,
/// because truncation and appending both show up precisely there. Interior
/// offsets are spread evenly and then rounded down to a block boundary; the
/// step is at least one extent wide, so rounding cannot make two extents
/// overlap or reorder.
pub(crate) fn sampled_extents(size: u64) -> Vec<(u64, u64)> {
    if size <= SAMPLE_WHOLE_FILE_LIMIT {
        return if size == 0 {
            Vec::new()
        } else {
            vec![(0, size)]
        };
    }
    let last = size - SAMPLE_EXTENT_BYTES;
    let step = last / (SAMPLE_EXTENTS - 1);
    (0..SAMPLE_EXTENTS)
        .map(|index| {
            let offset = if index == SAMPLE_EXTENTS - 1 {
                last
            } else {
                step.saturating_mul(index) / SAMPLE_ALIGN * SAMPLE_ALIGN
            };
            (offset, SAMPLE_EXTENT_BYTES)
        })
        .collect()
}

/// Hash a bounded sample of a source rather than every byte of it.
///
/// The layout itself is hashed before any content: the domain token, the
/// file's size, the extent width, the extent count, and then each extent's
/// offset and length ahead of its bytes. Two files that happen to read the
/// same bytes under different layouts therefore cannot collide, and a file
/// that grew changes digest even when every extent it kept is identical.
///
/// A short read is a hard error. The `after` identity re-check would catch a
/// size change anyway, but a digest computed over fewer bytes than it claims
/// must never be recorded as if it covered them.
async fn sampled_source_digest(
    source: &mut tokio::fs::File,
    size: u64,
    path: &Path,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<String, String> {
    let extents = sampled_extents(size);
    let mut digest = Sha256::new();
    digest.update(SAMPLE_DOMAIN);
    digest.update(size.to_be_bytes());
    digest.update(SAMPLE_EXTENT_BYTES.to_be_bytes());
    digest.update(
        u32::try_from(extents.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    let mut buffer = vec![0_u8; HASH_CHUNK];
    let mut read_total = 0_u64;
    for (offset, len) in extents {
        digest.update(offset.to_be_bytes());
        digest.update(len.to_be_bytes());
        source
            .seek(SeekFrom::Start(offset))
            .await
            .map_err(|error| format!("seeking {}: {error}", path.display()))?;
        let mut remaining = len;
        while remaining > 0 {
            let want = usize::try_from(remaining.min(HASH_CHUNK as u64)).unwrap_or(HASH_CHUNK);
            let read = source
                .read(&mut buffer[..want])
                .await
                .map_err(|error| format!("hashing {}: {error}", path.display()))?;
            if read == 0 {
                return Err("source ended before its attested size".to_owned());
            }
            digest.update(&buffer[..read]);
            remaining -= read as u64;
            read_total += read as u64;
        }
        progress(read_total);
    }
    Ok(hex::encode(digest.finalize()))
}

pub(crate) struct AttestedSource {
    pub(crate) handle: std::fs::File,
    pub(crate) observation: FragmentIndexSourceObservation,
}

pub(crate) struct SourceFence {
    pub(crate) handle: std::fs::File,
    object_version: String,
}

impl SourceFence {
    pub(crate) fn unchanged(&self) -> bool {
        self.handle
            .metadata()
            .ok()
            .and_then(|metadata| object_version(&metadata).ok())
            .is_some_and(|version| version == self.object_version)
    }
}

pub(crate) async fn open_source_fence(
    file: &MediaFile,
    expected_object_version: Option<&str>,
) -> Result<SourceFence, String> {
    let source = tokio::fs::File::open(&file.path)
        .await
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?;
    let metadata = source
        .metadata()
        .await
        .map_err(|error| format!("fstat {}: {error}", file.path.display()))?;
    let object_version = object_version(&metadata)?;
    if let Some(expected) = expected_object_version {
        scanner_identity_matches(&metadata, file)?;
        if expected != object_version {
            return Err("source changed after cluster index resolution".to_owned());
        }
    }
    Ok(SourceFence {
        handle: source.into_std().await,
        object_version,
    })
}

pub(crate) fn cache_root(cache_dir: &Path) -> PathBuf {
    cache_dir.join("fragment-index-v2")
}

fn cache_path(root: &Path, cache_key: &str) -> Option<PathBuf> {
    valid_digest(cache_key).then(|| root.join(&cache_key[..2]).join(format!("{cache_key}.idx")))
}

pub(crate) async fn remove_local_blob(root: &Path, cache_key: &str) -> bool {
    let Some(path) = cache_path(root, cache_key) else {
        return false;
    };
    match tokio::fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(cache_key, %error, "removing retired fragment-index blob");
            false
        }
    }
}

/// Reconcile one cursor-bounded cache-prefix page with the authoritative
/// catalog. The returned cursor advances through all 256 digest prefixes and
/// within a busy prefix, so valid entries at the head cannot permanently hide
/// later orphans. Any catalog read failure aborts the sweep and preserves the
/// current candidate.
pub(crate) async fn sweep_local_orphans(
    store: &dyn Store,
    root: &Path,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(usize, String), String> {
    const STAGING_GRACE: Duration = Duration::from_secs(60 * 60);

    let (prefix, after) = cursor
        .and_then(|cursor| cursor.split_once('/'))
        .and_then(|(prefix, after)| {
            u8::from_str_radix(prefix, 16)
                .ok()
                .map(|prefix| (prefix, after))
        })
        .unwrap_or((0, ""));
    let directory = root.join(format!("{prefix:02x}"));
    let mut files = match tokio::fs::read_dir(&directory).await {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, format!("{:02x}/", prefix.wrapping_add(1))));
        }
        Err(error) => return Err(format!("list {}: {error}", directory.display())),
    };
    let mut candidates = Vec::new();
    while let Some(file) = files
        .next_entry()
        .await
        .map_err(|error| format!("walk {}: {error}", directory.display()))?
    {
        if !file
            .file_type()
            .await
            .map_err(|error| error.to_string())?
            .is_file()
        {
            continue;
        }
        let Some(name) = file.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_index = name.strip_suffix(".idx").is_some_and(valid_digest);
        if name.as_str() > after && (is_index || name.ends_with(".tmp")) {
            candidates.push((name, file.path()));
        }
    }
    candidates.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let page_len = limit.max(1).min(candidates.len());
    let exhausted = page_len == candidates.len();
    let mut removed = 0_usize;
    let mut last = after.to_owned();
    for (name, path) in candidates.into_iter().take(page_len) {
        last = name.clone();
        if let Some(cache_key) = name.strip_suffix(".idx") {
            if store
                .cluster_fragment_index_artifact(cache_key)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
                && match tokio::fs::remove_file(&path).await {
                    Ok(()) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        return Err(format!("remove {}: {error}", path.display()));
                    }
                }
            {
                removed += 1;
            }
        } else if name.ends_with(".tmp") {
            let stale = tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= STAGING_GRACE);
            if stale {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("remove {}: {error}", path.display())),
                }
            }
        }
    }
    let next = if exhausted {
        format!("{:02x}/", prefix.wrapping_add(1))
    } else {
        format!("{prefix:02x}/{last}")
    };
    Ok((removed, next))
}

/// Open and cryptographically verify the exact descriptor that will be
/// streamed to a peer. Cache publication uses rename, so seeking this handle
/// back to zero keeps the response bound to the inode that was attested even
/// if another generation is installed at the pathname concurrently.
pub(crate) async fn open_verified_local_blob(
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<Option<tokio::fs::File>, String> {
    let Some(path) = cache_path(root, &artifact.cache_key) else {
        return Err("invalid fragment-index cache key".to_owned());
    };
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    let expected = u64::try_from(artifact.bytes)
        .ok()
        .filter(|bytes| *bytes <= MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES as u64)
        .ok_or_else(|| "fragment-index catalog has an invalid blob size".to_owned())?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| format!("fstat {}: {error}", path.display()))?;
    if metadata.len() != expected {
        return Err(format!(
            "fragment-index blob size is {}, expected {expected}",
            metadata.len()
        ));
    }
    let mut digest = Sha256::new();
    let mut seen = 0_u64;
    let mut buffer = vec![0_u8; HASH_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("verify {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        seen = seen.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }
    if seen != expected
        || !hex::encode(digest.finalize()).eq_ignore_ascii_case(&artifact.blob_sha256)
    {
        return Err("fragment-index blob checksum or size mismatch".to_owned());
    }
    file.seek(SeekFrom::Start(0))
        .await
        .map_err(|error| format!("rewind {}: {error}", path.display()))?;
    Ok(Some(file))
}

pub(crate) async fn read_local_blob(
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<Option<Vec<u8>>, String> {
    let Some(path) = cache_path(root, &artifact.cache_key) else {
        return Err("invalid fragment-index cache key".to_owned());
    };
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("stat {}: {error}", path.display())),
    };
    let expected = usize::try_from(artifact.bytes)
        .ok()
        .filter(|bytes| *bytes <= MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES)
        .ok_or_else(|| "fragment-index catalog has an invalid blob size".to_owned())?;
    if metadata.len() != expected as u64 {
        return Err(format!(
            "fragment-index blob size is {}, expected {expected}",
            metadata.len()
        ));
    }
    let blob = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    validate_blob(&blob, artifact)?;
    Ok(Some(blob))
}

pub(crate) async fn install_local_blob(
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
    blob: &[u8],
) -> Result<(), String> {
    validate_blob(blob, artifact)?;
    let Some(path) = cache_path(root, &artifact.cache_key) else {
        return Err("invalid fragment-index cache key".to_owned());
    };
    let parent = path
        .parent()
        .ok_or_else(|| "fragment-index cache path has no parent".to_owned())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    if let Ok(Some(existing)) = read_local_blob(root, artifact).await {
        if existing == blob {
            return Ok(());
        }
    }
    let staging = parent.join(format!(
        ".{}.{}.tmp",
        artifact.cache_key,
        uuid::Uuid::new_v4()
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options
        .open(&staging)
        .await
        .map_err(|error| format!("create {}: {error}", staging.display()))?;
    use tokio::io::AsyncWriteExt;
    if let Err(error) = file.write_all(blob).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("write {}: {error}", staging.display()));
    }
    if let Err(error) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("sync {}: {error}", staging.display()));
    }
    drop(file);
    if tokio::fs::rename(&staging, &path).await.is_err() {
        if read_local_blob(root, artifact).await?.is_some() {
            let _ = tokio::fs::remove_file(&staging).await;
            return Ok(());
        }
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::rename(&staging, &path)
            .await
            .map_err(|error| format!("publish {}: {error}", path.display()))?;
    }
    Ok(())
}

fn validate_blob(blob: &[u8], artifact: &ClusterFragmentIndexArtifact) -> Result<(), String> {
    if blob.len() > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES
        || blob.len() != usize::try_from(artifact.bytes).unwrap_or(usize::MAX)
        || !cluster_fragment_index_blob_sha256(blob).eq_ignore_ascii_case(&artifact.blob_sha256)
    {
        return Err("fragment-index blob checksum or size mismatch".to_owned());
    }
    decode_cluster_fragment_index_blob(blob, &artifact.source_sha256, &artifact.pipeline_sha256)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn decode_artifact(
    blob: &[u8],
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<FragmentIndex, String> {
    validate_blob(blob, artifact)?;
    decode_cluster_fragment_index_blob(blob, &artifact.source_sha256, &artifact.pipeline_sha256)
        .map_err(|error| error.to_string())
}

/// Prove that the file on this node's disk is the one the queue asked about,
/// and give the caller an open handle to it.
///
/// What this attests is *identity*: `object_version` — device, inode, size,
/// mtime and ctime to the nanosecond — is taken before the read and checked
/// again after it, and the scanner's own size and mtime are checked against
/// both. What the digest adds on top of that is change detection for a
/// rewrite that somehow preserved all of it, which is why it does not need to
/// cover every byte: it samples 64 megabyte-wide extents (the whole file
/// below 64 MiB), at a fixed cost regardless of how large the file is.
pub(crate) async fn attest_source(
    node_id: &str,
    file: &MediaFile,
    memo: Option<&FragmentIndexSourceObservation>,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<AttestedSource, String> {
    let mut source = tokio::fs::File::open(&file.path)
        .await
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?;
    let before = source
        .metadata()
        .await
        .map_err(|error| format!("fstat {}: {error}", file.path.display()))?;
    scanner_identity_matches(&before, file)?;
    let version = object_version(&before)?;
    let source_sha256 = if let Some(memo) = memo.filter(|memo| {
        memo.node_id == node_id
            && memo.file_id == file.id
            && memo.object_version == version
            && memo.source_size == file.size
            && memo.source_mtime == file.mtime
            && valid_digest(&memo.source_sha256)
    }) {
        memo.source_sha256.clone()
    } else {
        let digest = sampled_source_digest(&mut source, before.len(), &file.path, progress).await?;
        let after = source
            .metadata()
            .await
            .map_err(|error| format!("re-fstat {}: {error}", file.path.display()))?;
        scanner_identity_matches(&after, file)?;
        if object_version(&after)? != version {
            return Err("source changed while its digest was read".to_owned());
        }
        digest
    };
    source
        .seek(SeekFrom::Start(0))
        .await
        .map_err(|error| format!("rewinding {}: {error}", file.path.display()))?;
    Ok(AttestedSource {
        handle: source.into_std().await,
        observation: FragmentIndexSourceObservation {
            node_id: node_id.to_owned(),
            file_id: file.id,
            object_version: version,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256,
            observed_at_ms: unix_ms(),
        },
    })
}

pub(crate) async fn inspect_source(file: &MediaFile) -> Result<String, String> {
    let metadata = tokio::fs::metadata(&file.path)
        .await
        .map_err(|error| format!("stat {}: {error}", file.path.display()))?;
    scanner_identity_matches(&metadata, file)?;
    object_version(&metadata)
}

pub(crate) fn source_still_matches(
    source: &std::fs::File,
    observation: &FragmentIndexSourceObservation,
) -> Result<bool, String> {
    let metadata = source
        .metadata()
        .map_err(|error| format!("re-fstat attested source: {error}"))?;
    Ok(metadata.len() == observation.source_size.max(0) as u64
        && object_version(&metadata)? == observation.object_version)
}

fn scanner_identity_matches(metadata: &std::fs::Metadata, file: &MediaFile) -> Result<(), String> {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    if metadata.len() != file.size.max(0) as u64 || modified != file.mtime {
        return Err("source no longer matches the scanner's size/mtime identity".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
fn object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
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
fn object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
    let modified = metadata
        .modified()
        .map_err(|error| format!("reading source modification time: {error}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("source modification time precedes unix epoch: {error}"))?;
    Ok(format!("{}:{}", metadata.len(), modified.as_nanos()))
}

pub(crate) fn pipeline_digest(
    file: &MediaFile,
    engine_sha256: &str,
    video: plurx_core::transcode::CopyVideoOptions,
) -> String {
    let mut args = plurx_core::transcode::copy_index_pipe_args(file, video);
    if let Some(input) = args
        .windows(2)
        .position(|window| window[0] == "-i")
        .map(|index| index + 1)
    {
        args[input] = "{attested-source-fd}".to_owned();
    }
    cluster_fragment_index_pipeline_digest(engine_sha256, &args)
        .expect("the engine probe always returns a SHA-256 digest")
}

pub(crate) async fn hydrate(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<Option<FragmentIndex>, String> {
    match read_local_blob(root, artifact).await {
        Ok(Some(blob)) => {
            let index = decode_artifact(&blob, artifact)?;
            publish_location(store, node_id, artifact).await?;
            return Ok(Some(index));
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                cache_key = artifact.cache_key,
                %error,
                "discarding a corrupt local fragment-index blob"
            );
            if let Some(path) = cache_path(root, &artifact.cache_key) {
                let _ = tokio::fs::remove_file(path).await;
            }
            let _ = store
                .forget_cluster_fragment_index_location(&artifact.cache_key, node_id)
                .await;
        }
    }
    let Some(membership) = membership else {
        return Ok(None);
    };
    let peers = membership
        .media_peers()
        .await
        .map_err(|error| format!("listing fragment-index peers: {error}"))?
        .into_iter()
        .filter_map(|peer| {
            peer.http_base
                .map(|base| (peer.node_id, (base, peer.reachable)))
        })
        .filter(|(peer_node, (_, reachable))| peer_node != node_id && *reachable)
        .collect::<HashMap<_, _>>();
    let locations = store
        .cluster_fragment_index_locations(&artifact.cache_key)
        .await
        .map_err(|error| error.to_string())?;
    let transport = PeerTransport::new(membership.clone());
    let path = format!("{PEER_PATH_PREFIX}{}", artifact.cache_key);
    for location in locations {
        let Some((base, _)) = peers.get(&location.node_id) else {
            continue;
        };
        let deadline = tokio::time::Instant::now() + PEER_DEADLINE;
        let response = transport
            .request(
                &location.node_id,
                base,
                reqwest::Method::GET,
                &path,
                Vec::new(),
                deadline,
                MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await;
        let Ok(response) = response else { continue };
        if response.status == reqwest::StatusCode::NOT_FOUND {
            let _ = store
                .forget_cluster_fragment_index_location(&artifact.cache_key, &location.node_id)
                .await;
            continue;
        }
        if !response.status.is_success() {
            continue;
        }
        if validate_blob(&response.body, artifact).is_err() {
            let _ = store
                .forget_cluster_fragment_index_location(&artifact.cache_key, &location.node_id)
                .await;
            continue;
        }
        install_local_blob(root, artifact, &response.body).await?;
        publish_location(store, node_id, artifact).await?;
        return decode_artifact(&response.body, artifact).map(Some);
    }
    Ok(None)
}

pub(crate) async fn publish_location(
    store: &dyn Store,
    node_id: &str,
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<(), String> {
    let now = unix_ms();
    store
        .put_cluster_fragment_index_location(&ClusterFragmentIndexLocation {
            cache_key: artifact.cache_key.clone(),
            node_id: node_id.to_owned(),
            bytes: artifact.bytes,
            verified_at_ms: now,
            last_seen_at_ms: now,
        })
        .await
        .map_err(|error| error.to_string())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_accepts_only_a_digest() {
        let root = Path::new("/cache");
        let key = "a".repeat(64);
        assert_eq!(
            cache_path(root, &key),
            Some(root.join("aa").join(format!("{key}.idx")))
        );
        assert!(cache_path(root, "../escape").is_none());
    }

    #[tokio::test]
    async fn orphan_sweep_cursor_advances_past_a_full_head_page() {
        let store = plurx_core::store::SqliteStore::open_in_memory().expect("store");
        let cache = tempfile::tempdir().expect("cache");
        let root = cache.path();
        let prefix = root.join("00");
        tokio::fs::create_dir_all(&prefix)
            .await
            .expect("prefix directory");
        for sequence in 0_u64..257 {
            let key = format!("00{sequence:062x}");
            tokio::fs::write(prefix.join(format!("{key}.idx")), b"orphan")
                .await
                .expect("orphan");
        }

        let (first_removed, cursor) = sweep_local_orphans(&store, root, None, 256)
            .await
            .expect("first page");
        assert_eq!(first_removed, 256);
        assert!(cursor.starts_with("00/"));
        let (second_removed, cursor) = sweep_local_orphans(&store, root, Some(&cursor), 256)
            .await
            .expect("second page");
        assert_eq!(second_removed, 1);
        assert_eq!(cursor, "01/");
    }

    // ---- sampled source attestation -----------------------------------

    fn sampled_file(path: std::path::PathBuf) -> MediaFile {
        let metadata = std::fs::metadata(&path).expect("sample metadata");
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or(0);
        MediaFile {
            id: 41,
            item_id: 7,
            path,
            size: metadata.len() as i64,
            mtime,
            duration_ms: Some(1_000),
            container: Some("mkv".to_owned()),
            video_codec: Some("hevc".to_owned()),
            video_profile: Some("Main 10".to_owned()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            dolby_vision: plurx_core::domain::DolbyVisionFacts::default(),
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        }
    }

    /// Deterministic filler, so a flipped byte is the only difference between
    /// two files and not an artifact of how they were written.
    fn filler(size: usize) -> Vec<u8> {
        (0..size)
            .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    async fn digest_of(path: &Path) -> String {
        let mut source = tokio::fs::File::open(path).await.expect("open sample");
        let size = source.metadata().await.expect("stat sample").len();
        sampled_source_digest(&mut source, size, path, &|_| {})
            .await
            .expect("sampled digest")
    }

    #[test]
    fn sampled_extents_layout() {
        assert!(
            sampled_extents(0).is_empty(),
            "an empty file has no extents"
        );

        // At the limit the whole file is one extent; a single byte more and
        // the file is sampled.
        assert_eq!(
            sampled_extents(SAMPLE_WHOLE_FILE_LIMIT),
            vec![(0, SAMPLE_WHOLE_FILE_LIMIT)]
        );
        let size = SAMPLE_WHOLE_FILE_LIMIT + 1;
        let extents = sampled_extents(size);
        assert_eq!(extents.len() as u64, SAMPLE_EXTENTS);

        // A realistic file, where the interior offsets are far apart enough
        // for alignment to be visible.
        let size = 43_u64 * 1024 * 1024 * 1024;
        let extents = sampled_extents(size);
        assert_eq!(extents.len() as u64, SAMPLE_EXTENTS);
        assert_eq!(extents[0], (0, SAMPLE_EXTENT_BYTES), "the head is at zero");
        assert_eq!(
            extents[extents.len() - 1],
            (size - SAMPLE_EXTENT_BYTES, SAMPLE_EXTENT_BYTES),
            "the tail ends exactly at the end of the file"
        );
        let mut previous = None;
        for (index, (offset, len)) in extents.iter().copied().enumerate() {
            assert_eq!(len, SAMPLE_EXTENT_BYTES, "extent {index} is a full width");
            assert!(offset + len <= size, "extent {index} runs past the file");
            if index + 1 < extents.len() {
                assert_eq!(
                    offset % SAMPLE_ALIGN,
                    0,
                    "interior extent {index} is aligned"
                );
            }
            if let Some(previous) = previous {
                assert!(
                    offset >= previous + SAMPLE_EXTENT_BYTES,
                    "extent {index} overlaps its predecessor"
                );
            }
            previous = Some(offset);
        }
        // The whole point of the fixed count: a file three orders of
        // magnitude larger costs the same read.
        let total: u64 = extents.iter().map(|(_, len)| len).sum();
        assert_eq!(total, SAMPLE_WHOLE_FILE_LIMIT);
    }

    #[tokio::test]
    async fn sampled_digest_is_domain_separated() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("sampled.bin");
        let size = 70 * 1024 * 1024;
        let bytes = filler(size);
        tokio::fs::write(&path, &bytes).await.expect("write sample");

        let sampled = digest_of(&path).await;
        assert_eq!(sampled.len(), 64, "the digest stays a sha256 by shape");
        assert!(sampled.chars().all(|c| c.is_ascii_hexdigit()));

        let whole = hex::encode(Sha256::digest(&bytes));
        assert_ne!(
            sampled, whole,
            "a sampled digest must never collide with a whole-file one"
        );

        // Nor with a bare hash of the same bytes in the same order: the
        // layout preamble is what separates them.
        let mut concatenated = Sha256::new();
        for (offset, len) in sampled_extents(size as u64) {
            let start = offset as usize;
            concatenated.update(&bytes[start..start + len as usize]);
        }
        assert_ne!(sampled, hex::encode(concatenated.finalize()));
    }

    #[tokio::test]
    async fn sampled_digest_sees_a_flip_in_any_extent() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("flip.bin");
        let size = 70 * 1024 * 1024;
        let bytes = filler(size);
        tokio::fs::write(&path, &bytes).await.expect("write sample");
        let baseline = digest_of(&path).await;

        let extents = sampled_extents(size as u64);
        let interior = extents[extents.len() / 2].0 as usize;
        for (label, at) in [
            ("head", 0_usize),
            ("interior", interior + 7),
            ("tail", size - 1),
        ] {
            let mut flipped = bytes.clone();
            flipped[at] ^= 0x01;
            tokio::fs::write(&path, &flipped).await.expect("write flip");
            assert_ne!(
                digest_of(&path).await,
                baseline,
                "a flip in the {label} extent must change the digest"
            );
        }

        // The accepted trade, pinned so nobody "fixes" it by accident: a
        // change strictly between two sampled extents is not seen by the
        // digest. `object_version` is what actually guards this file, and it
        // moves on any write.
        let covered = extents
            .iter()
            .map(|(offset, len)| (*offset as usize, (offset + len) as usize))
            .collect::<Vec<_>>();
        let gap = covered
            .windows(2)
            .find_map(|pair| (pair[1].0 > pair[0].1 + 1).then(|| pair[0].1 + 1))
            .expect("a sampled 70 MiB file has gaps between its extents");
        let mut untouched = bytes.clone();
        untouched[gap] ^= 0x01;
        tokio::fs::write(&path, &untouched)
            .await
            .expect("write gap");
        assert_eq!(
            digest_of(&path).await,
            baseline,
            "a flip between extents is deliberately invisible to the digest"
        );
    }

    #[tokio::test]
    async fn sampled_digest_sees_growth() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("grow.bin");
        let size = 70 * 1024 * 1024;
        let bytes = filler(size);
        tokio::fs::write(&path, &bytes).await.expect("write sample");
        let baseline = digest_of(&path).await;

        let mut grown = bytes.clone();
        grown.push(0x5a);
        tokio::fs::write(&path, &grown).await.expect("write grown");
        assert_ne!(
            digest_of(&path).await,
            baseline,
            "size is in the preamble, so a file that grew changes digest"
        );
    }

    #[tokio::test]
    async fn attest_source_reports_progress_and_costs_a_fixed_read() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("progress.bin");
        let size = 70 * 1024 * 1024;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("write sample");
        let file = sampled_file(path);

        let seen = std::sync::Mutex::new(Vec::new());
        let attested = attest_source("progress-node", &file, None, &|bytes| {
            seen.lock().expect("progress lock").push(bytes);
        })
        .await
        .expect("attest sampled source");
        assert_eq!(attested.observation.source_sha256.len(), 64);

        let seen = seen.into_inner().expect("progress values");
        assert_eq!(
            seen.len() as u64,
            SAMPLE_EXTENTS,
            "one report per extent read"
        );
        assert!(
            seen.windows(2).all(|pair| pair[1] > pair[0]),
            "progress must be monotonic: {seen:?}"
        );
        assert_eq!(
            seen.last().copied(),
            Some(SAMPLE_WHOLE_FILE_LIMIT),
            "a 70 MiB source costs exactly the sample, not the file"
        );
    }

    #[tokio::test]
    async fn attest_source_refuses_a_short_source() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("short.bin");
        let size = 70 * 1024 * 1024;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("write sample");
        // The scanner's identity is taken while the file is whole, so the
        // attestation begins believing the file is 70 MiB.
        let file = sampled_file(path.clone());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen for truncate")
            .set_len(4 * 1024 * 1024)
            .expect("truncate");

        let error = attest_source("short-node", &file, None, &|_| {})
            .await
            .err()
            .expect("a truncated source cannot be attested");
        assert!(
            !error.is_empty(),
            "the refusal has to say something an operator can read"
        );
    }
}
