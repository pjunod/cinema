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
/// How far a whole-file digest reads between checkpoints a later attempt may
/// resume from. At the ~105 MB/s a NAS mount gives, that is under a second of
/// rereading after a preemption.
#[cfg(not(test))]
const FULL_DIGEST_CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const FULL_DIGEST_CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;
/// Only a handful of HEVC sources are ever part-way through attestation on one
/// node, so this bound is never reached in practice; it keeps the table from
/// growing without limit if sources keep changing under their attempts.
const FULL_DIGEST_CHECKPOINT_LIMIT: usize = 16;

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
/// Domain separation inside the digest. A sampled digest can never equal a
/// whole-file SHA-256 of the same bytes, nor a sampled digest taken under a
/// different layout.
const SAMPLE_DOMAIN: &[u8] = b"plurx/source-attestation/sampled-v1\0";

/// The attestation regime, stamped onto every `object_version` this build
/// records and compares.
///
/// This is what actually makes a layout change migrate. A memo is matched by
/// `object_version` alone — the table has no column saying how its digest was
/// computed — so without this prefix, changing [`SAMPLE_DOMAIN`] would rehash
/// nothing on any node that already holds an observation, and two nodes could
/// hold different digests for the same file indefinitely. Different digests
/// mean different `cluster_fragment_index_key`s, which means the same file
/// gets built and stored twice and neither node can hydrate the other's
/// artifact. `object_version` never changes on a stable library, so nothing
/// would ever heal it.
///
/// Prefixing instead makes every pre-existing memo miss exactly once. The
/// node re-attests — two seconds now, rather than the forty-three minutes
/// that made this change necessary — and the upsert replaces the row. Move
/// this token whenever [`SAMPLE_DOMAIN`] or the extent layout moves.
const ATTESTATION_REGIME: &str = "s1";

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

/// How many bytes attesting a source of this size will actually read.
///
/// The verify stage's progress bar and ETA are denominated in this rather
/// than the file's own size: a 43 GB source reads 64 MiB, and a bar against
/// the file would sit at a tenth of a percent while quoting twenty minutes
/// for an operation that finishes in two seconds.
#[must_use]
pub(crate) fn attestation_read_bytes(size: u64) -> u64 {
    sampled_extents(size).iter().map(|(_, len)| len).sum()
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
    pub(crate) fn object_version(&self) -> &str {
        &self.object_version
    }

    /// A second demuxer needs an independent file description: on macOS,
    /// opening /dev/fd/N duplicates its seek offset, unlike Linux procfs.
    /// Reopen by name only after checking the complete held object identity.
    pub(crate) async fn reopen(&self, file: &MediaFile) -> Result<Self, String> {
        open_source_fence(file, Some(&self.object_version)).await
    }

    pub(crate) fn unchanged(&self) -> bool {
        self.drift().is_none()
    }

    /// What moved under this fence, in the operator's terms, or `None` if
    /// nothing did.
    ///
    /// `unchanged` returning a bare `false` was not enough to act on. It is
    /// the answer to three different situations — the source really was
    /// rewritten, the source's inode metadata moved without its bytes changing
    /// (a second link, or a name unlinked), or the `fstat` itself failed — and
    /// they need opposite responses. The failure it produces says "source
    /// changed before fragment publication", which is a confident sentence
    /// about a thing that may not have happened, and there was no way from
    /// outside to tell which case a refusal came from.
    ///
    /// Naming the component makes the refusal a diagnosis. Sizes and times
    /// come from the same `fstat` the object version is built from, so this
    /// adds no syscall to the hot path beyond the one already there.
    pub(crate) fn drift(&self) -> Option<String> {
        #[cfg(unix)]
        let metadata = match self.handle.metadata() {
            Ok(metadata) => metadata,
            // A fence whose own descriptor cannot be stated is not evidence
            // that the source changed. It is refused just the same, because
            // publishing against a source nothing can vouch for is worse, but
            // it is refused under its own name.
            Err(error) => {
                return Some(format!(
                    "the source descriptor could not be stated: {error}"
                ))
            }
        };
        #[cfg(unix)]
        let current = object_version(&metadata);
        #[cfg(windows)]
        let current = windows_object_version(&self.handle);
        let current = match current {
            Ok(version) => version,
            Err(error) => return Some(format!("the source identity could not be read: {error}")),
        };
        if current == self.object_version {
            return None;
        }
        Some(describe_object_version_drift(
            &self.object_version,
            &current,
        ))
    }
}

/// The difference between two object versions, field by field.
///
/// Written out rather than printing both strings because the fields are what
/// carry the meaning: `size` moving means the bytes changed, `mtime` moving
/// means they were written, `ctime` moving *alone* means only the inode's
/// metadata moved — a permission change, or a link added or removed — which is
/// a source that did not change at all. An operator reading "ctime" and
/// nothing else knows to look for whatever is relinking the file rather than
/// for whatever is rewriting it.
fn describe_object_version_drift(before: &str, after: &str) -> String {
    const FIELDS: [&str; 8] = [
        "regime",
        "dev",
        "ino",
        "size",
        "mtime",
        "mtime_nsec",
        "ctime",
        "ctime_nsec",
    ];
    let before_parts: Vec<&str> = before.split(':').collect();
    let after_parts: Vec<&str> = after.split(':').collect();
    if before_parts.len() != after_parts.len() {
        return format!("source identity is a different shape: {before} became {after}");
    }
    let moved: Vec<String> = before_parts
        .iter()
        .zip(after_parts.iter())
        .enumerate()
        .filter(|(_, (was, now))| was != now)
        .map(|(index, (was, now))| {
            let field = FIELDS.get(index).copied().unwrap_or("field");
            format!("{field} {was} -> {now}")
        })
        .collect();
    if moved.is_empty() {
        // Unreachable for two strings that compared unequal, and stated rather
        // than unwrapped so a future field-count change cannot turn into a
        // silent "nothing moved" on a refusal.
        return format!("source identity differs but no field does: {before} vs {after}");
    }
    moved.join(", ")
}

pub(crate) async fn open_source_fence(
    file: &MediaFile,
    expected_object_version: Option<&str>,
) -> Result<SourceFence, String> {
    #[cfg(unix)]
    {
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
    #[cfg(windows)]
    {
        let path = file.path.clone();
        let source = tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&path)
        })
        .await
        .map_err(|error| format!("opening source task failed: {error}"))?
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?;
        let metadata = source
            .metadata()
            .map_err(|error| format!("stating {}: {error}", file.path.display()))?;
        scanner_identity_matches(&metadata, file)?;
        let object_version = windows_object_version(&source)?;
        if expected_object_version.is_some_and(|expected| expected != object_version) {
            return Err("source changed after cluster index resolution".to_owned());
        }
        Ok(SourceFence {
            handle: source,
            object_version,
        })
    }
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
        plurx_core::process::bounded::check_cancellation().map_err(|error| error.to_string())?;
        let remaining = expected.saturating_add(1).saturating_sub(seen);
        if remaining == 0 {
            return Err("fragment-index blob grew beyond its manifest".into());
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..limit])
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
    attest_source_mode(
        node_id,
        file,
        memo,
        progress,
        false,
        AttestationIo::default(),
    )
    .await
}

/// Copy proofs may cross nodes only after every byte is attested. Keep this
/// regime separate from subtitle/sample observations and their memo keys.
/// Production reads go through [`attest_copy_source_with`], which carries the
/// durable checkpoints (and, on the request path, the cap).
#[cfg(test)]
pub(crate) async fn attest_copy_source(
    node_id: &str,
    file: &MediaFile,
    memo: Option<&FragmentIndexSourceObservation>,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<AttestedSource, String> {
    attest_copy_source_with(node_id, file, memo, progress, AttestationIo::default()).await
}

/// [`attest_copy_source`] with durable checkpoints and, on the request path,
/// the byte-rate cap. See [`AttestationIo`].
pub(crate) async fn attest_copy_source_with(
    node_id: &str,
    file: &MediaFile,
    memo: Option<&FragmentIndexSourceObservation>,
    progress: &(dyn Fn(u64) + Sync),
    io: AttestationIo<'_>,
) -> Result<AttestedSource, String> {
    attest_source_mode(node_id, file, memo, progress, is_hevc(file), io).await
}

fn is_hevc(file: &MediaFile) -> bool {
    matches!(file.video_codec.as_deref(), Some("hevc" | "h265"))
}

const FULL_COPY_PREFIX: &str = "hevc-full-v1:";

pub(crate) fn local_object_version(version: &str) -> &str {
    version.strip_prefix(FULL_COPY_PREFIX).unwrap_or(version)
}

pub(crate) async fn inspect_copy_source(file: &MediaFile) -> Result<String, String> {
    let version = inspect_source(file).await?;
    Ok(if is_hevc(file) {
        format!("{FULL_COPY_PREFIX}{version}")
    } else {
        version
    })
}

pub(crate) fn copy_attestation_read_bytes(file: &MediaFile) -> u64 {
    let size = file.size.max(0) as u64;
    if is_hevc(file) {
        size
    } else {
        attestation_read_bytes(size)
    }
}

/// A SHA-256 whose state can be written down and picked up again.
///
/// `sha2 0.10`'s `Sha256` keeps its chaining value, partial block and byte
/// count private and has no export, so a whole-file digest could only ever be
/// resumed inside the process that started it. This is the same function
/// built on the crate's own compression primitive (`compress256`, behind its
/// `compress` feature), with the three pieces of state in the open. The
/// partial block is the part that matters: the domain token and size prefix
/// in front of the file's bytes mean a 64 MiB checkpoint is never on a block
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumableSha256 {
    state: [u32; 8],
    block: [u8; 64],
    /// Bytes fed so far; the last `length % 64` of them wait in `block`.
    length: u64,
}

/// FIPS 180-4 §5.3.3.
const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];
/// Serialized layout: format byte, eight big-endian state words, the
/// big-endian byte count, then the `length % 64` buffered bytes.
const RESUMABLE_SHA256_FORMAT: u8 = 1;
const RESUMABLE_SHA256_FIXED_BYTES: usize = 1 + 32 + 8;

type Sha256Block = sha2::digest::generic_array::GenericArray<u8, sha2::digest::consts::U64>;

impl Default for ResumableSha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl ResumableSha256 {
    pub(crate) fn new() -> Self {
        Self {
            state: SHA256_INITIAL_STATE,
            block: [0; 64],
            length: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        let buffered = (self.length % 64) as usize;
        self.length = self.length.wrapping_add(data.len() as u64);
        if buffered > 0 {
            let take = (64 - buffered).min(data.len());
            self.block[buffered..buffered + take].copy_from_slice(&data[..take]);
            data = &data[take..];
            if buffered + take < 64 {
                return;
            }
            sha2::compress256(&mut self.state, &[*Sha256Block::from_slice(&self.block)]);
        }
        let whole = data.len() / 64 * 64;
        let (blocks, rest) = data.split_at(whole);
        // `compress256` takes a slice of blocks; batching amortises its CPU
        // feature dispatch across 4 KiB rather than paying it per block.
        let mut batch = [Sha256Block::default(); 64];
        for chunk in blocks.chunks(64 * 64) {
            let count = chunk.len() / 64;
            for (slot, block) in batch.iter_mut().zip(chunk.chunks_exact(64)) {
                slot.copy_from_slice(block);
            }
            sha2::compress256(&mut self.state, &batch[..count]);
        }
        self.block[..rest.len()].copy_from_slice(rest);
    }

    pub(crate) fn finalize(self) -> [u8; 32] {
        let buffered = (self.length % 64) as usize;
        let mut tail = [0_u8; 128];
        tail[..buffered].copy_from_slice(&self.block[..buffered]);
        tail[buffered] = 0x80;
        let blocks = if buffered < 56 { 1 } else { 2 };
        tail[blocks * 64 - 8..blocks * 64]
            .copy_from_slice(&self.length.wrapping_mul(8).to_be_bytes());
        let mut state = self.state;
        for block in tail[..blocks * 64].chunks_exact(64) {
            sha2::compress256(&mut state, &[*Sha256Block::from_slice(block)]);
        }
        let mut out = [0_u8; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Bytes fed so far.
    pub(crate) fn len(&self) -> u64 {
        self.length
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let buffered = (self.length % 64) as usize;
        let mut out = Vec::with_capacity(RESUMABLE_SHA256_FIXED_BYTES + buffered);
        out.push(RESUMABLE_SHA256_FORMAT);
        for word in self.state {
            out.extend_from_slice(&word.to_be_bytes());
        }
        out.extend_from_slice(&self.length.to_be_bytes());
        out.extend_from_slice(&self.block[..buffered]);
        out
    }

    /// The inverse of [`Self::to_bytes`]; `None` for anything it did not
    /// write, including a buffered tail of the wrong length.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (&format, rest) = bytes.split_first()?;
        if format != RESUMABLE_SHA256_FORMAT || rest.len() < 40 {
            return None;
        }
        let mut state = [0_u32; 8];
        for (word, raw) in state.iter_mut().zip(rest[..32].chunks_exact(4)) {
            *word = u32::from_be_bytes(raw.try_into().ok()?);
        }
        let length = u64::from_be_bytes(rest[32..40].try_into().ok()?);
        let tail = &rest[40..];
        if tail.len() != (length % 64) as usize {
            return None;
        }
        let mut block = [0_u8; 64];
        block[..tail.len()].copy_from_slice(tail);
        Some(Self {
            state,
            block,
            length,
        })
    }
}

/// The domain token and size prefix every whole-file HEVC digest starts with.
const FULL_DIGEST_DOMAIN: &[u8] = b"plurx/source-attestation/hevc-full-v1\0";

fn full_digest_prefix_len() -> u64 {
    FULL_DIGEST_DOMAIN.len() as u64 + 8
}

/// The object a whole-file digest is being computed for. A checkpoint is
/// only ever resumed by an attempt whose freshly observed key is identical.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FullDigestKey {
    node_id: String,
    file_id: i64,
    /// The full-regime `object_version` (device, inode, size, mtime and ctime
    /// to the nanosecond) taken before the checkpointed bytes were read.
    object_version: String,
    size: u64,
}

/// How far a whole-file HEVC digest had got when its attempt was dropped.
#[derive(Clone)]
struct FullDigestCheckpoint {
    key: FullDigestKey,
    offset: u64,
    digest: ResumableSha256,
}

/// Progress of whole-file digests whose attempts were preempted.
///
/// A background attestation stops whenever this node starts playback that is
/// not waiting on it, and the attempt's future is simply dropped. Before this
/// table the next attempt began again at byte 0, so a source that takes longer
/// to read than the gap between two playbacks never finished. File 5208, a
/// 79.5 GB remux at ~105 MB/s (12.6 minutes), was preempted 53 times without
/// once completing, and every play of it fell back to rolling HLS.
///
/// Resuming does not weaken what is attested. Every byte is still hashed, in
/// order, into one SHA-256 state. A checkpoint is accepted only by an attempt
/// whose own `object_version`, taken when it opens the file and checked again
/// after its last byte, equals the version the checkpointing attempt saw when
/// it opened the file. Any write in between moves ctime, so the resume is
/// refused. That is the same premise an uninterrupted read relies on between
/// its first and last byte.
///
/// A finished digest is kept as a checkpoint at the end of the file. The
/// caller can still discard the result after the hash completes: playback
/// may preempt it before it is recorded, or the claim may be lost. The retry
/// then reruns both identity checks and skips the read. Once the result is
/// recorded, the source memo serves later attempts and the entry ages out of
/// the bound.
///
/// This table is the fast path. Each checkpoint is also written to the
/// node-local store ([`AttestationIo::checkpoints`]), so a restarted daemon —
/// which starts with this table empty — resumes from the stored row instead
/// of byte 0. With a byte-rate cap a whole-file read beside playback takes
/// tens of minutes, and this fleet restarts several times a day.
#[derive(Default)]
struct FullDigestCheckpoints {
    entries: Vec<FullDigestCheckpoint>,
}

impl FullDigestCheckpoints {
    /// A copy of the checkpoint for exactly this object, if there is one.
    ///
    /// The entry stays in place: an attempt that is itself preempted before
    /// its first save must not take the earlier progress down with it. An
    /// entry for the same node and file under a different identity can never
    /// be resumed and is dropped.
    fn load(&mut self, key: &FullDigestKey) -> Option<FullDigestCheckpoint> {
        self.entries.retain(|entry| {
            entry.key == *key
                || entry.key.node_id != key.node_id
                || entry.key.file_id != key.file_id
        });
        self.entries
            .iter()
            .find(|entry| entry.key == *key)
            .filter(|entry| entry.offset <= key.size)
            .cloned()
    }

    fn save(&mut self, checkpoint: FullDigestCheckpoint) {
        // Two attempts at one identity at once each hold a valid state. Keep
        // the one that has read further.
        if self
            .entries
            .iter()
            .any(|entry| entry.key == checkpoint.key && entry.offset >= checkpoint.offset)
        {
            return;
        }
        self.entries.retain(|entry| {
            entry.key.node_id != checkpoint.key.node_id
                || entry.key.file_id != checkpoint.key.file_id
        });
        if self.entries.len() >= FULL_DIGEST_CHECKPOINT_LIMIT {
            self.entries.remove(0);
        }
        self.entries.push(checkpoint);
    }

    fn clear(&mut self, key: &FullDigestKey) {
        self.entries.retain(|entry| entry.key != *key);
    }
}

static FULL_DIGEST_CHECKPOINTS: std::sync::LazyLock<std::sync::Mutex<FullDigestCheckpoints>> =
    std::sync::LazyLock::new(Default::default);

fn full_digest_checkpoints() -> std::sync::MutexGuard<'static, FullDigestCheckpoints> {
    FULL_DIGEST_CHECKPOINTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How often a paced read asks whether the node is still serving anything.
pub(crate) const PACE_EVALUATE_BYTES: u64 = 4 * 1024 * 1024;

/// Whether this node is serving anything right now — the pacer's trigger.
pub(crate) trait AttestationLoad: Send + Sync {
    fn serving(&self) -> futures_util::future::BoxFuture<'_, bool>;
}

/// A byte-rate cap on one whole-file source read, engaged while the node is
/// serving anything (QSF Part D, ruling R7).
///
/// The trigger is evaluated every [`PACE_EVALUATE_BYTES`]; while it holds,
/// each read waits for its turn so reads start at least `len / rate` apart.
/// Over any window of `W` seconds the read therefore stays within
/// `rate × W` plus one chunk, and it returns to full speed at the first
/// evaluation after the last delivery ends. There is no starvation test:
/// `readrate` is zero for unpaced and cached sessions and `recent_speed` is
/// `None` after every respawn, so "is the producer starved" cannot be
/// answered — "is anything playing" can.
pub(crate) struct AttestationPacer<'a> {
    load: &'a dyn AttestationLoad,
    bytes_per_sec: u64,
    file_id: i64,
    state: std::sync::Mutex<PacerState>,
    capped: std::sync::atomic::AtomicBool,
    capped_seen: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct PacerState {
    next_evaluation: Option<u64>,
    next_free: Option<tokio::time::Instant>,
}

impl<'a> AttestationPacer<'a> {
    pub(crate) fn new(load: &'a dyn AttestationLoad, bytes_per_sec: u64, file_id: i64) -> Self {
        Self {
            load,
            bytes_per_sec: bytes_per_sec.max(1),
            file_id,
            state: std::sync::Mutex::new(PacerState::default()),
            capped: std::sync::atomic::AtomicBool::new(false),
            capped_seen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Whether the cap is holding the read right now.
    pub(crate) fn capped(&self) -> bool {
        self.capped.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether the cap held the read at any point. A deadline reached by a
    /// read the cap slowed is not evidence against the source.
    pub(crate) fn was_capped(&self) -> bool {
        self.capped_seen.load(std::sync::atomic::Ordering::Acquire)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PacerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until `len` bytes starting at `offset` may be read.
    async fn admit(&self, offset: u64, len: usize) {
        let evaluate = self.lock().next_evaluation.is_none_or(|at| offset >= at);
        if evaluate {
            let serving = self.load.serving().await;
            let was = self
                .capped
                .swap(serving, std::sync::atomic::Ordering::AcqRel);
            if serving {
                self.capped_seen
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            if serving != was {
                if serving {
                    tracing::info!(
                        file_id = self.file_id,
                        offset,
                        bytes_per_sec = self.bytes_per_sec,
                        "attestation read cap on: this node is serving playback"
                    );
                } else {
                    tracing::info!(
                        file_id = self.file_id,
                        offset,
                        "attestation read cap off: nothing is playing from this node"
                    );
                }
            }
            let mut state = self.lock();
            state.next_evaluation = Some(offset.saturating_add(PACE_EVALUATE_BYTES));
            if !serving {
                state.next_free = None;
            }
        }
        if !self.capped() {
            return;
        }
        let start = {
            let mut state = self.lock();
            let now = tokio::time::Instant::now();
            let start = state.next_free.map_or(now, |free| free.max(now));
            state.next_free =
                Some(start + Duration::from_secs_f64(len as f64 / self.bytes_per_sec as f64));
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

/// What a whole-file attestation may use beyond the file itself.
#[derive(Clone, Copy, Default)]
pub(crate) struct AttestationIo<'a> {
    /// Node-local durable checkpoints. Both the request and the job path pass
    /// one, so either resumes after a restart; `None` keeps the in-memory
    /// table only.
    pub(crate) checkpoints: Option<&'a dyn Store>,
    /// The byte-rate cap. The request path only: the job path has no viewer
    /// waiting on it, so it stops for playback instead of slowing down.
    pub(crate) pacer: Option<&'a AttestationPacer<'a>>,
}

/// The stored checkpoint for this key, if one survives and decodes into a
/// state that covers exactly the prefix plus `read_offset` bytes.
async fn durable_checkpoint(
    io: AttestationIo<'_>,
    key: &FullDigestKey,
) -> Option<FullDigestCheckpoint> {
    let store = io.checkpoints?;
    let size = i64::try_from(key.size).ok()?;
    let row = match store
        .attestation_checkpoint(&key.node_id, key.file_id, &key.object_version, size)
        .await
    {
        Ok(row) => row?,
        Err(error) => {
            tracing::warn!(file_id = key.file_id, %error, "reading a stored attestation checkpoint");
            return None;
        }
    };
    let offset = u64::try_from(row.read_offset).ok()?;
    match ResumableSha256::from_bytes(&row.hasher_state) {
        Some(digest)
            if offset <= key.size
                && digest.len() == full_digest_prefix_len().saturating_add(offset) =>
        {
            Some(FullDigestCheckpoint {
                key: key.clone(),
                offset,
                digest,
            })
        }
        _ => {
            tracing::warn!(
                file_id = key.file_id,
                "discarding a stored attestation checkpoint that does not decode"
            );
            let _ = store
                .forget_attestation_checkpoint(&key.node_id, key.file_id)
                .await;
            None
        }
    }
}

async fn save_durable_checkpoint(io: AttestationIo<'_>, checkpoint: &FullDigestCheckpoint) {
    let Some(store) = io.checkpoints else {
        return;
    };
    let (Ok(source_size), Ok(read_offset)) = (
        i64::try_from(checkpoint.key.size),
        i64::try_from(checkpoint.offset),
    ) else {
        return;
    };
    // Best effort: the in-memory table still holds this progress, and the
    // next checkpoint writes again.
    if let Err(error) = store
        .put_attestation_checkpoint(&plurx_core::store::AttestationCheckpoint {
            node_id: checkpoint.key.node_id.clone(),
            file_id: checkpoint.key.file_id,
            object_version: checkpoint.key.object_version.clone(),
            source_size,
            read_offset,
            hasher_state: checkpoint.digest.to_bytes(),
            saved_at_ms: unix_ms(),
        })
        .await
    {
        tracing::warn!(file_id = checkpoint.key.file_id, %error, "saving an attestation checkpoint");
    }
}

async fn full_source_digest(
    source: &mut tokio::fs::File,
    key: &FullDigestKey,
    progress: &(dyn Fn(u64) + Sync),
    io: AttestationIo<'_>,
) -> Result<String, String> {
    let size = key.size;
    let in_memory = full_digest_checkpoints().load(key);
    let (resume, durable) = match in_memory {
        Some(checkpoint) => (Some(checkpoint), false),
        None => (durable_checkpoint(io, key).await, true),
    };
    let (mut digest, mut offset) = match resume {
        Some(checkpoint) => {
            source
                .seek(SeekFrom::Start(checkpoint.offset))
                .await
                .map_err(|e| format!("resuming complete HEVC source digest: {e}"))?;
            tracing::info!(
                file_id = key.file_id,
                offset = checkpoint.offset,
                size,
                durable,
                "resuming a preempted whole-file source attestation"
            );
            progress(checkpoint.offset);
            (checkpoint.digest, checkpoint.offset)
        }
        None => {
            let mut digest = ResumableSha256::new();
            digest.update(FULL_DIGEST_DOMAIN);
            digest.update(&size.to_be_bytes());
            (digest, 0)
        }
    };
    let mut buffer = vec![0; HASH_CHUNK];
    let mut next_checkpoint = offset.saturating_add(FULL_DIGEST_CHECKPOINT_BYTES);
    let mut mark = (tokio::time::Instant::now(), offset);
    while offset < size {
        let want = (size - offset).min(HASH_CHUNK as u64) as usize;
        if let Some(pacer) = io.pacer {
            pacer.admit(offset, want).await;
        }
        let read = match source.read(&mut buffer[..want]).await {
            Ok(0) => {
                full_digest_checkpoints().clear(key);
                if let Some(store) = io.checkpoints {
                    let _ = store
                        .forget_attestation_checkpoint(&key.node_id, key.file_id)
                        .await;
                }
                return Err("source ended before full attestation".into());
            }
            Ok(read) => read,
            // The bytes already hashed are still the bytes of this identity,
            // and the next attempt re-checks the identity before using them.
            // A transient NFS error must not throw away tens of gigabytes.
            Err(e) => return Err(format!("hashing complete HEVC source: {e}")),
        };
        digest.update(&buffer[..read]);
        offset += read as u64;
        progress(offset);
        if offset >= next_checkpoint || offset == size {
            let checkpoint = FullDigestCheckpoint {
                key: key.clone(),
                offset,
                digest: digest.clone(),
            };
            full_digest_checkpoints().save(checkpoint.clone());
            save_durable_checkpoint(io, &checkpoint).await;
            next_checkpoint = offset.saturating_add(FULL_DIGEST_CHECKPOINT_BYTES);
            // The measured rate since the last checkpoint. Without it nothing
            // in the log can show whether the cap ran or what it held to.
            let now = tokio::time::Instant::now();
            let elapsed = now.saturating_duration_since(mark.0).as_secs_f64();
            let read_bytes_per_sec = if elapsed > 0.0 {
                ((offset - mark.1) as f64 / elapsed) as u64
            } else {
                0
            };
            mark = (now, offset);
            match io.pacer {
                Some(pacer) => tracing::info!(
                    file_id = key.file_id,
                    offset,
                    size,
                    read_bytes_per_sec,
                    capped = pacer.capped(),
                    "attestation checkpoint"
                ),
                None => tracing::debug!(
                    file_id = key.file_id,
                    offset,
                    size,
                    read_bytes_per_sec,
                    "attestation checkpoint"
                ),
            }
        }
    }
    Ok(hex::encode(digest.finalize()))
}

async fn attest_source_mode(
    node_id: &str,
    file: &MediaFile,
    memo: Option<&FragmentIndexSourceObservation>,
    progress: &(dyn Fn(u64) + Sync),
    full: bool,
    io: AttestationIo<'_>,
) -> Result<AttestedSource, String> {
    #[cfg(unix)]
    let mut source = tokio::fs::File::open(&file.path)
        .await
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?;
    #[cfg(windows)]
    let source_handle = {
        let path = file.path.clone();
        tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&path)
        })
        .await
        .map_err(|error| format!("opening source task failed: {error}"))?
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?
    };
    #[cfg(windows)]
    let mut source = tokio::fs::File::from_std(
        source_handle
            .try_clone()
            .map_err(|error| format!("cloning held source {}: {error}", file.path.display()))?,
    );
    let before = source
        .metadata()
        .await
        .map_err(|error| format!("fstat {}: {error}", file.path.display()))?;
    scanner_identity_matches(&before, file)?;
    #[cfg(unix)]
    let version = object_version(&before)?;
    #[cfg(windows)]
    let version = windows_object_version(&source_handle)?;
    let version = if full {
        format!("{FULL_COPY_PREFIX}{version}")
    } else {
        version
    };
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
        let digest = if full {
            let key = FullDigestKey {
                node_id: node_id.to_owned(),
                file_id: file.id,
                object_version: version.clone(),
                size: before.len(),
            };
            full_source_digest(&mut source, &key, progress, io).await?
        } else {
            sampled_source_digest(&mut source, before.len(), &file.path, progress).await?
        };
        let after = source
            .metadata()
            .await
            .map_err(|error| format!("re-fstat {}: {error}", file.path.display()))?;
        scanner_identity_matches(&after, file)?;
        #[cfg(unix)]
        let after_version = object_version(&after)?;
        #[cfg(windows)]
        let after_version = windows_object_version(&source_handle)?;
        if after_version != local_object_version(&version) {
            return Err("source changed while its digest was read".to_owned());
        }
        digest
    };
    source
        .seek(SeekFrom::Start(0))
        .await
        .map_err(|error| format!("rewinding {}: {error}", file.path.display()))?;
    Ok(AttestedSource {
        #[cfg(unix)]
        handle: source.into_std().await,
        #[cfg(windows)]
        handle: source_handle,
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
    #[cfg(unix)]
    {
        let metadata = tokio::fs::metadata(&file.path)
            .await
            .map_err(|error| format!("stat {}: {error}", file.path.display()))?;
        scanner_identity_matches(&metadata, file)?;
        object_version(&metadata)
    }
    #[cfg(windows)]
    {
        let path = file.path.clone();
        let source = tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&path)
        })
        .await
        .map_err(|error| format!("opening source task failed: {error}"))?
        .map_err(|error| format!("opening {}: {error}", file.path.display()))?;
        let metadata = source
            .metadata()
            .map_err(|error| format!("stating {}: {error}", file.path.display()))?;
        scanner_identity_matches(&metadata, file)?;
        windows_object_version(&source)
    }
}

pub(crate) fn source_still_matches(
    source: &std::fs::File,
    observation: &FragmentIndexSourceObservation,
) -> Result<bool, String> {
    let metadata = source
        .metadata()
        .map_err(|error| format!("re-fstat attested source: {error}"))?;
    #[cfg(unix)]
    let version = object_version(&metadata)?;
    #[cfg(windows)]
    let version = windows_object_version(source)?;
    Ok(metadata.len() == observation.source_size.max(0) as u64
        && version == local_object_version(&observation.object_version))
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
        "{ATTESTATION_REGIME}:{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(windows)]
fn windows_object_version(file: &std::fs::File) -> Result<String, String> {
    let identity = plurx_core::fs_secure::std_file_identity(file)
        .map_err(|error| format!("reading Windows source FileIdInfo: {error}"))?;
    Ok(format!(
        "{ATTESTATION_REGIME}:{}:{}:{}:{}:{}:{}",
        identity.device,
        identity.inode,
        identity.inode_high,
        identity.size,
        identity.changed_seconds,
        identity.changed_nanoseconds
    ))
}

/// A source's identity as the subtitle-source store keys it: size and mtime
/// for every consumer, and `(dev, ino)` for the burn path.
///
/// Deliberately not [`SourceFence::object_version`], which also carries ctime.
/// A hardlink or `chmod` from an importer moves ctime without touching a
/// byte, and a store keyed on it would miss on that file for good while
/// nothing ever re-rode the pass that fills it. `(dev, ino)` is what rejects a
/// file replaced in place with a new inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceStamp {
    pub(crate) size: u64,
    /// Whole seconds, as the scanner records `files.mtime`.
    pub(crate) mtime: i64,
    #[serde(default)]
    pub(crate) dev: Option<u64>,
    #[serde(default)]
    pub(crate) ino: Option<u64>,
}

#[cfg(unix)]
pub(crate) fn source_stamp(metadata: &std::fs::Metadata) -> SourceStamp {
    use std::os::unix::fs::MetadataExt;
    SourceStamp {
        size: metadata.size(),
        mtime: metadata.mtime(),
        dev: Some(metadata.dev()),
        ino: Some(metadata.ino()),
    }
}

/// The Windows port has no `(dev, ino)` in `std::fs::Metadata`, so the store
/// falls back to size + mtime there, the overlay's rule.
#[cfg(not(unix))]
pub(crate) fn source_stamp(metadata: &std::fs::Metadata) -> SourceStamp {
    SourceStamp {
        size: metadata.len(),
        mtime: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or(0),
        dev: None,
        ino: None,
    }
}

pub(crate) fn pipeline_digest(
    file: &MediaFile,
    engine_sha256: &str,
    video: plurx_core::transcode::CopyVideoOptions,
) -> String {
    pipeline_digest_for_transform(
        file,
        engine_sha256,
        video,
        crate::fragindex::output_transform_identity(file, video),
    )
}

fn pipeline_digest_for_transform(
    file: &MediaFile,
    engine_sha256: &str,
    video: plurx_core::transcode::CopyVideoOptions,
    transform: Option<&str>,
) -> String {
    let mut args = plurx_core::transcode::copy_index_pipe_args(file, video);
    if matches!(file.video_codec.as_deref(), Some("hevc" | "h265")) {
        args.push(format!(
            "{{plurx-hevc-proof:{}}}",
            plurx_core::hevc_configuration::REVISION
        ));
    }
    if let Some(input) = args
        .windows(2)
        .position(|window| window[0] == "-i")
        .map(|index| index + 1)
    {
        args[input] = "{attested-source-fd}".to_owned();
    }
    // The conversion is a Rust transform after FFmpeg, so it is absent from
    // the executable argv above.  Bind its revision into the same canonical
    // recipe vector before hashing; otherwise a changed converter could reuse
    // old fragment sizes and init metadata under a still-valid engine digest.
    if let (true, Some(transform)) = (video.converts_dolby_vision(), transform) {
        args.push(format!("{{plurx-output-transform:{transform}}}"));
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
    hydrate_inner(store, membership, node_id, root, artifact, true).await
}

pub(crate) async fn hydrate_for_job(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
) -> Result<Option<FragmentIndex>, String> {
    hydrate_inner(store, membership, node_id, root, artifact, false).await
}

async fn hydrate_inner(
    store: &dyn Store,
    membership: Option<&MembershipManager>,
    node_id: &str,
    root: &Path,
    artifact: &ClusterFragmentIndexArtifact,
    publish: bool,
) -> Result<Option<FragmentIndex>, String> {
    match read_local_blob(root, artifact).await {
        Ok(Some(blob)) => {
            let index = decode_artifact(&blob, artifact)?;
            if publish {
                publish_location(store, node_id, artifact).await?;
            }
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
            // Best-effort: deleting the corrupt blob already prevents reuse;
            // reconciliation can retire the stale catalogue location.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetCorruptLocalIndex,
                crate::store_result::Discard::BestEffort,
                store
                    .forget_cluster_fragment_index_location(&artifact.cache_key, node_id)
                    .await,
            );
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
            // Best-effort: this peer has already been excluded from the
            // current hydration attempt; later repair retries stale cleanup.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetMissingPeerIndex,
                crate::store_result::Discard::BestEffort,
                store
                    .forget_cluster_fragment_index_location(&artifact.cache_key, &location.node_id)
                    .await,
            );
            continue;
        }
        if !response.status.is_success() {
            continue;
        }
        if validate_blob(&response.body, artifact).is_err() {
            // Best-effort: the invalid blob is never installed, and future
            // reconciliation can remove this peer's stale location row.
            crate::store_result::observe(
                crate::store_result::Operation::ForgetCorruptPeerIndex,
                crate::store_result::Discard::BestEffort,
                store
                    .forget_cluster_fragment_index_location(&artifact.cache_key, &location.node_id)
                    .await,
            );
            continue;
        }
        install_local_blob(root, artifact, &response.body).await?;
        if publish {
            publish_location(store, node_id, artifact).await?;
        }
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

/// An HEVC catalogue row for a real file, as the scanner would record it,
/// for tests in this crate that exercise the whole-file regime.
#[cfg(test)]
pub(crate) fn hevc_test_file(id: i64, path: PathBuf) -> MediaFile {
    let metadata = std::fs::metadata(&path).expect("HEVC test file metadata");
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id,
        item_id: 7,
        path,
        size: metadata.len() as i64,
        mtime,
        duration_ms: Some(1_000),
        container: Some("mkv".to_owned()),
        video_codec: Some("hevc".to_owned()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".to_owned()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: plurx_core::domain::DolbyVisionFacts::default(),
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
    }
}

#[cfg(test)]
mod tests {
    /// The refusal has to say which component moved, because the three things
    /// it covers need opposite responses and the sentence in front of it —
    /// "source changed" — is only true for one of them.
    ///
    /// A `ctime` that moves alone is the case worth naming: the bytes, the
    /// size and the modification time are all still exactly what was attested,
    /// and what actually happened is that a name was added to or removed from
    /// the inode. Reported as "source changed" that sends an operator looking
    /// for whatever is rewriting a file nothing is writing to.
    #[test]
    fn a_refusal_names_the_component_that_moved() {
        let before = "s1:66:1234:5000:1700:0:1700:0";

        let relinked = "s1:66:1234:5000:1700:0:1900:5";
        assert_eq!(
            super::describe_object_version_drift(before, relinked),
            "ctime 1700 -> 1900, ctime_nsec 0 -> 5",
            "only the inode metadata moved, and only that is reported"
        );

        let rewritten = "s1:66:1234:6000:1900:0:1900:0";
        assert_eq!(
            super::describe_object_version_drift(before, rewritten),
            "size 5000 -> 6000, mtime 1700 -> 1900, ctime 1700 -> 1900",
            "a source that really was rewritten reads as one"
        );

        let replaced = "s1:66:9999:5000:1700:0:1700:0";
        assert_eq!(
            super::describe_object_version_drift(before, replaced),
            "ino 1234 -> 9999",
            "a different file under the same name is an inode change, not a write"
        );

        let regime = "s2:66:1234:5000:1700:0:1700:0";
        assert_eq!(
            super::describe_object_version_drift(before, regime),
            "regime s1 -> s2",
            "an attestation-layout migration is not a source change either"
        );

        // A shape change would otherwise index the wrong field names onto the
        // wrong values, which is worse than saying nothing.
        assert_eq!(
            super::describe_object_version_drift(before, "s1:66:1234"),
            "source identity is a different shape: s1:66:1234:5000:1700:0:1700:0 became s1:66:1234",
        );
    }

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

    /// The shared key is portable across host paths, exact across all three
    /// Dolby Vision recipes, and includes the Rust transform revision that is
    /// deliberately absent from FFmpeg's executable argv.
    #[test]
    fn pipeline_digest_is_portable_and_exact_for_dolby_vision_recipes() {
        let dir = tempfile::tempdir().expect("source dir");
        let path = dir.path().join("movie.mkv");
        std::fs::write(&path, b"source").expect("source");
        let mut file = sampled_file(path);
        file.hdr = Some("dolby_vision".to_owned());
        file.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);
        let engine = "a".repeat(64);

        let stripped = plurx_core::transcode::CopyVideoOptions::new(true, false);
        let preserved = plurx_core::transcode::CopyVideoOptions::new(true, true);
        let converted = preserved.with_dolby_vision_conversion(true);
        let keys =
            [stripped, preserved, converted].map(|video| pipeline_digest(&file, &engine, video));
        assert_eq!(
            keys.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );

        let mut relocated = file.clone();
        relocated.path = PathBuf::from("/a/different/host/mount/movie.mkv");
        assert_eq!(
            pipeline_digest(&file, &engine, converted),
            pipeline_digest(&relocated, &engine, converted),
            "the attested source digest, not a host-local path, supplies source identity"
        );

        let current = pipeline_digest(&file, &engine, converted);
        let next = pipeline_digest_for_transform(
            &file,
            &engine,
            converted,
            Some("dv-p7-to-p81-rpu-next-test-revision"),
        );
        assert_ne!(current, next);
        assert_eq!(
            pipeline_digest(&file, &engine, preserved),
            pipeline_digest_for_transform(
                &file,
                &engine,
                preserved,
                Some("dv-p7-to-p81-rpu-next-test-revision")
            ),
            "an unrelated conversion revision must not invalidate ordinary artifacts"
        );
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
            downloaded_subtitles: Vec::new(),
            id: 41,
            item_id: 7,
            path,
            size: metadata.len() as i64,
            mtime,
            duration_ms: Some(1_000),
            container: Some("mkv".to_owned()),
            video_codec: Some("hevc".to_owned()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".to_owned()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
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
        assert_eq!(attestation_read_bytes(size), SAMPLE_WHOLE_FILE_LIMIT);
        assert_eq!(attestation_read_bytes(0), 0);
        assert_eq!(attestation_read_bytes(4_096), 4_096);
        assert_eq!(
            attestation_read_bytes(SAMPLE_WHOLE_FILE_LIMIT),
            SAMPLE_WHOLE_FILE_LIMIT
        );

        // Exhaustive over the awkward band, where `step` is closest to one
        // extent width and a rounded-down offset is most likely to collide
        // with its predecessor.
        for size in (SAMPLE_WHOLE_FILE_LIMIT + 1)..(SAMPLE_WHOLE_FILE_LIMIT + 8_192) {
            let extents = sampled_extents(size);
            assert_eq!(extents.len() as u64, SAMPLE_EXTENTS, "size {size}");
            for pair in extents.windows(2) {
                assert!(
                    pair[1].0 >= pair[0].0 + pair[0].1,
                    "size {size}: {:?} overlaps {:?}",
                    pair[1],
                    pair[0]
                );
            }
            let (offset, len) = extents[extents.len() - 1];
            assert_eq!(offset + len, size, "size {size}: the tail must end at EOF");
        }
        // And at the far end, where the arithmetic is widest.
        for size in [u64::MAX, u64::MAX - 1, 1 << 47, (1 << 47) + 4_097] {
            let extents = sampled_extents(size);
            assert_eq!(extents.len() as u64, SAMPLE_EXTENTS, "size {size}");
            assert!(extents
                .windows(2)
                .all(|pair| pair[1].0 >= pair[0].0 + pair[0].1));
            let (offset, len) = extents[extents.len() - 1];
            assert_eq!(offset + len, size, "size {size}");
        }
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
    async fn hevc_full_attestation_observes_changes_outside_the_old_sample() {
        let dir = tempfile::tempdir().expect("HEVC regression fixture");
        let path = dir.path().join("copy-source.bin");
        let size = 70 * 1024 * 1024;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("HEVC regression fixture");
        let file = sampled_file(path.clone());
        let sampled = attest_source("node", &file, None, &|_| {})
            .await
            .expect("HEVC regression fixture");
        let progress = std::sync::atomic::AtomicU64::new(0);
        let full = attest_copy_source("node", &file, Some(&sampled.observation), &|bytes| {
            progress.store(bytes, std::sync::atomic::Ordering::Relaxed);
        })
        .await
        .expect("HEVC regression fixture");
        assert_eq!(
            progress.load(std::sync::atomic::Ordering::Relaxed),
            size as u64
        );
        assert_eq!(copy_attestation_read_bytes(&file), size as u64);
        assert_ne!(
            full.observation.source_sha256,
            sampled.observation.source_sha256
        );
        assert_eq!(
            full.observation.object_version,
            inspect_copy_source(&file)
                .await
                .expect("HEVC regression fixture")
        );
        assert!(
            source_still_matches(&full.handle, &full.observation).expect("HEVC regression fixture")
        );
        // A memo in the full regime is reused without another whole-file read.
        let memo = attest_copy_source("node", &file, Some(&full.observation), &|_| {
            panic!("memo must avoid I/O")
        })
        .await
        .expect("HEVC regression fixture");
        assert_eq!(
            memo.observation.source_sha256,
            full.observation.source_sha256
        );
        let offset = sampled_extents(size as u64)[0].1;
        assert!(offset < sampled_extents(size as u64)[1].0);
        use tokio::io::AsyncWriteExt;
        let mut writer = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .expect("HEVC regression fixture");
        writer
            .seek(SeekFrom::Start(offset))
            .await
            .expect("HEVC regression fixture");
        writer
            .write_all(&[0xff])
            .await
            .expect("HEVC regression fixture");
        writer.flush().await.expect("HEVC regression fixture");
        let changed = sampled_file(path.clone());
        let sampled_after = attest_source("node", &changed, None, &|_| {})
            .await
            .expect("HEVC regression fixture");
        let full_after = attest_copy_source("node", &changed, None, &|_| {})
            .await
            .expect("HEVC regression fixture");
        assert_eq!(
            sampled_after.observation.source_sha256,
            sampled.observation.source_sha256
        );
        assert_ne!(
            full_after.observation.source_sha256,
            full.observation.source_sha256
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
    async fn a_short_source_is_an_error_and_never_a_shorter_digest() {
        // The guard this covers is the one inside the digest: asked to cover
        // a size the file does not have, it must refuse rather than record a
        // digest over fewer bytes than it claims. Reached directly, because
        // `attest_source`'s own identity checks would refuse a truncated file
        // long before the read — which is right, and is why they cannot be
        // the coverage for this.
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("short.bin");
        let real = 4 * 1024 * 1024;
        tokio::fs::write(&path, filler(real))
            .await
            .expect("write sample");
        let mut source = tokio::fs::File::open(&path).await.expect("open short");
        let claimed = 70 * 1024 * 1024;
        let error = sampled_source_digest(&mut source, claimed, &path, &|_| {})
            .await
            .expect_err("a source shorter than its claimed size cannot be digested");
        assert_eq!(error, "source ended before its attested size");
    }

    #[tokio::test]
    async fn attest_source_refuses_a_source_that_shrank_under_it() {
        let dir = tempfile::tempdir().expect("sample dir");
        let path = dir.path().join("shrank.bin");
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
        assert_eq!(
            error,
            "source no longer matches the scanner's size/mtime identity"
        );
    }

    #[test]
    fn every_object_version_carries_the_attestation_regime() {
        // The memo table has no column saying how a digest was computed, so
        // the regime has to live in the key the memo is matched by. Without
        // it, a node holding a pre-sampling observation keeps a whole-file
        // digest forever, gets a different cache key from every other node,
        // and neither can hydrate the other's artifact.
        let dir = tempfile::tempdir().expect("version dir");
        let path = dir.path().join("versioned.bin");
        std::fs::write(&path, b"versioned").expect("write");
        #[cfg(unix)]
        let version =
            object_version(&std::fs::metadata(&path).expect("stat")).expect("object version");
        #[cfg(windows)]
        let version = windows_object_version(&std::fs::File::open(&path).expect("open"))
            .expect("object version");
        assert!(
            version.starts_with(&format!("{ATTESTATION_REGIME}:")),
            "object_version must be regime-scoped, got {version}"
        );
        assert_eq!(ATTESTATION_REGIME, "s1");
    }

    fn full_key(node_id: &str, file: &MediaFile) -> FullDigestKey {
        #[cfg(unix)]
        let version =
            object_version(&std::fs::metadata(&file.path).expect("stat")).expect("object version");
        #[cfg(windows)]
        let version = windows_object_version(&std::fs::File::open(&file.path).expect("open"))
            .expect("object version");
        FullDigestKey {
            node_id: node_id.to_owned(),
            file_id: file.id,
            object_version: format!("{FULL_COPY_PREFIX}{version}"),
            size: file.size as u64,
        }
    }

    /// Run a whole-file attestation and drop it once it has read `past` bytes,
    /// the way a playback preemption drops the attempt future.
    async fn preempt_full_attestation(node_id: &str, file: &MediaFile, past: u64) {
        let stop = tokio::sync::Notify::new();
        let report = |bytes: u64| {
            if bytes >= past {
                stop.notify_one();
            }
        };
        tokio::select! {
            biased;
            () = stop.notified() => {}
            result = attest_copy_source(node_id, file, None, &report) => panic!(
                "the attempt was meant to be preempted, but it finished: {:?}",
                result.map(|attested| attested.observation.source_sha256)
            ),
        }
    }

    /// Records the first progress report, which on a resumed attempt is the
    /// offset it resumed from.
    async fn first_progress_and_digest(node_id: &str, file: &MediaFile) -> (u64, String) {
        let first = std::sync::Mutex::new(None);
        let attested = attest_copy_source(node_id, file, None, &|bytes| {
            first.lock().expect("progress lock").get_or_insert(bytes);
        })
        .await
        .expect("whole-file attestation");
        let first = first
            .into_inner()
            .expect("progress values")
            .expect("at least one progress report");
        (first, attested.observation.source_sha256)
    }

    /// The incident shape: a 79.5 GB source preempted every few minutes never
    /// finished, because each attempt began again at byte 0. A resumed
    /// attempt starts from the last checkpoint and still produces exactly the
    /// digest an uninterrupted read of every byte produces.
    #[tokio::test]
    async fn a_preempted_full_attestation_resumes_and_matches_an_uninterrupted_read() {
        let dir = tempfile::tempdir().expect("resume fixture");
        let path = dir.path().join("resume.bin");
        let size = 6 * FULL_DIGEST_CHECKPOINT_BYTES as usize + 1_234;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("resume fixture");
        let file = sampled_file(path);
        let (from_zero, reference) =
            first_progress_and_digest("resume-reference-node", &file).await;
        assert!(
            from_zero <= HASH_CHUNK as u64,
            "a fresh attempt starts at 0"
        );

        let node = "resume-node";
        let past = 2 * FULL_DIGEST_CHECKPOINT_BYTES + HASH_CHUNK as u64;
        preempt_full_attestation(node, &file, past).await;
        let (resumed_from, resumed) = first_progress_and_digest(node, &file).await;
        assert!(
            resumed_from >= 2 * FULL_DIGEST_CHECKPOINT_BYTES && resumed_from < size as u64,
            "the second attempt resumes at the checkpoint, not byte 0: {resumed_from}"
        );
        assert_eq!(
            resumed, reference,
            "resuming must not change what the digest covers"
        );

        // The caller may still discard a finished digest (playback preempts it
        // before it is recorded). The retry re-checks identity and reads nothing.
        let (again_from, again) = first_progress_and_digest(node, &file).await;
        assert_eq!(
            again_from, size as u64,
            "a finished digest is resumed at the end of the file"
        );
        assert_eq!(again, reference);
    }

    /// Two preemptions in a row: the second attempt is dropped before it
    /// reaches a checkpoint of its own, and the third must still resume from
    /// the first attempt's progress rather than from 0.
    #[tokio::test]
    async fn an_attempt_preempted_before_its_first_checkpoint_keeps_the_earlier_one() {
        let dir = tempfile::tempdir().expect("resume fixture");
        let path = dir.path().join("twice.bin");
        let size = 6 * FULL_DIGEST_CHECKPOINT_BYTES as usize + 77;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("resume fixture");
        let file = sampled_file(path);
        let node = "twice-node";
        preempt_full_attestation(node, &file, 2 * FULL_DIGEST_CHECKPOINT_BYTES + 1).await;
        // Resumes at >= 2 checkpoints and is dropped one chunk later, well
        // before 3 checkpoints.
        preempt_full_attestation(node, &file, 2 * FULL_DIGEST_CHECKPOINT_BYTES + 1).await;
        let (resumed_from, digest) = first_progress_and_digest(node, &file).await;
        assert!(
            resumed_from >= 2 * FULL_DIGEST_CHECKPOINT_BYTES,
            "the earlier progress survived the second preemption: {resumed_from}"
        );
        let (_, reference) = first_progress_and_digest("twice-reference-node", &file).await;
        assert_eq!(digest, reference);
    }

    /// A source rewritten between attempts has a different identity, so the
    /// checkpoint for the old bytes is refused and the new bytes are hashed
    /// from the start.
    #[tokio::test]
    async fn a_checkpoint_is_refused_once_the_source_identity_moves() {
        let dir = tempfile::tempdir().expect("resume fixture");
        let path = dir.path().join("rewritten.bin");
        let size = 6 * FULL_DIGEST_CHECKPOINT_BYTES as usize + 9;
        let original = filler(size);
        tokio::fs::write(&path, &original)
            .await
            .expect("resume fixture");
        let file = sampled_file(path.clone());
        let node = "rewrite-node";
        preempt_full_attestation(node, &file, 3 * FULL_DIGEST_CHECKPOINT_BYTES).await;
        let stale = full_key(node, &file);
        assert!(
            full_digest_checkpoints().load(&stale).is_some(),
            "the preempted attempt left a checkpoint"
        );

        // Same size, different bytes in the region the checkpoint already
        // covers. Only the identity can tell the attempt not to trust it.
        let mut rewritten = original;
        rewritten[17] ^= 0xff;
        tokio::fs::write(&path, &rewritten)
            .await
            .expect("rewrite fixture");
        let file = sampled_file(path);
        assert_ne!(full_key(node, &file), stale, "a rewrite moves the identity");

        let (resumed_from, digest) = first_progress_and_digest(node, &file).await;
        assert!(
            resumed_from <= HASH_CHUNK as u64,
            "the stale checkpoint must not be resumed: {resumed_from}"
        );
        let (_, reference) = first_progress_and_digest("rewrite-reference-node", &file).await;
        assert_eq!(digest, reference, "the new bytes are what is attested");
        assert!(
            full_digest_checkpoints().load(&stale).is_none(),
            "the unusable checkpoint is gone"
        );
    }

    #[test]
    fn the_checkpoint_table_is_bounded_and_keeps_one_entry_per_file() {
        let key = |file_id: i64, version: &str| FullDigestKey {
            node_id: "bound-node".to_owned(),
            file_id,
            object_version: version.to_owned(),
            size: 100,
        };
        let checkpoint = |key: FullDigestKey, offset: u64| FullDigestCheckpoint {
            key,
            offset,
            digest: ResumableSha256::new(),
        };
        let mut table = FullDigestCheckpoints::default();
        table.save(checkpoint(key(1, "v1"), 10));
        table.save(checkpoint(key(1, "v1"), 20));
        assert_eq!(
            table.entries.len(),
            1,
            "a later save replaces the earlier one"
        );
        assert_eq!(
            table.load(&key(1, "v1")).map(|entry| entry.offset),
            Some(20)
        );
        table.save(checkpoint(key(1, "v1"), 15));
        assert_eq!(
            table.load(&key(1, "v1")).map(|entry| entry.offset),
            Some(20),
            "a slower concurrent attempt does not undo the faster one's progress"
        );

        table.save(checkpoint(key(1, "v2"), 5));
        assert_eq!(table.entries.len(), 1, "one file has one current identity");
        assert!(table.load(&key(1, "v1")).is_none());

        for file_id in 2..=(FULL_DIGEST_CHECKPOINT_LIMIT as i64 + 1) {
            table.save(checkpoint(key(file_id, "v1"), 1));
        }
        assert_eq!(table.entries.len(), FULL_DIGEST_CHECKPOINT_LIMIT);
        assert!(
            table.load(&key(1, "v2")).is_none(),
            "the oldest entry is the one evicted"
        );
    }

    // ---- resumable SHA-256, pacing and durable checkpoints (QSF Part D) ----

    /// xorshift64*, so the splits are random but every run sees the same.
    fn next(state: &mut u64) -> u64 {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// D-M2 step 0: for random splits, including every awkward boundary
    /// around a block and lengths that are never block-aligned, a state
    /// written down with `to_bytes` and picked up with `from_bytes` at every
    /// split finalizes to exactly `Sha256::digest` of the whole input.
    #[test]
    fn a_resumed_sha256_equals_the_library_digest_for_random_splits() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let lengths = [
            0_usize, 1, 3, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129, 1_000, 4_095, 4_097,
            70_001, 262_147,
        ];
        for length in lengths {
            let input = filler(length);
            let expected: [u8; 32] = Sha256::digest(&input).into();
            for _ in 0..24 {
                let mut cuts = (0..(next(&mut seed) % 6))
                    .map(|_| (next(&mut seed) as usize) % (length + 1))
                    .collect::<Vec<_>>();
                cuts.push(length);
                cuts.sort_unstable();
                let mut hasher = ResumableSha256::new();
                let mut at = 0;
                for cut in cuts {
                    hasher.update(&input[at..cut]);
                    at = cut;
                    let bytes = hasher.to_bytes();
                    hasher =
                        ResumableSha256::from_bytes(&bytes).expect("a written state reads back");
                    assert_eq!(hasher.len(), at as u64);
                }
                assert_eq!(hasher.finalize(), expected, "length {length}");
            }
        }
        // The domain prefix every whole-file digest starts with is not
        // block-aligned, which is why the partial block has to persist.
        assert_ne!(full_digest_prefix_len() % 64, 0);
        assert!(ResumableSha256::from_bytes(&[]).is_none());
        assert!(
            ResumableSha256::from_bytes(&[2; 41]).is_none(),
            "unknown format"
        );
        let mut short = ResumableSha256::new();
        short.update(b"abc");
        let mut bytes = short.to_bytes();
        bytes.pop();
        assert!(
            ResumableSha256::from_bytes(&bytes).is_none(),
            "a buffered tail shorter than the byte count says is refused"
        );
    }

    struct FlagLoad(std::sync::atomic::AtomicBool);

    impl AttestationLoad for FlagLoad {
        fn serving(&self) -> futures_util::future::BoxFuture<'_, bool> {
            let serving = self.0.load(std::sync::atomic::Ordering::Acquire);
            Box::pin(async move { serving })
        }
    }

    /// D-M1: while the node is serving, a capped read never exceeds the
    /// configured rate over any 5 s window; at the first evaluation after the
    /// last delivery ends it returns to full speed.
    #[tokio::test(start_paused = true)]
    async fn a_capped_read_holds_its_rate_and_returns_to_full_speed() {
        let dir = tempfile::tempdir().expect("pacing fixture");
        let path = dir.path().join("paced.bin");
        let size = 6 * FULL_DIGEST_CHECKPOINT_BYTES as usize + 1_234;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("pacing fixture");
        let file = hevc_test_file(4_101, path);
        let rate = 1024 * 1024_u64;
        let release_at = 4 * PACE_EVALUATE_BYTES;
        let load = FlagLoad(std::sync::atomic::AtomicBool::new(true));
        let pacer = AttestationPacer::new(&load, rate, file.id);
        let samples = std::sync::Mutex::new(Vec::new());
        let started = tokio::time::Instant::now();
        let attested = attest_copy_source_with(
            "pacing-node",
            &file,
            None,
            &|bytes| {
                samples
                    .lock()
                    .expect("samples")
                    .push((tokio::time::Instant::now(), bytes));
                if bytes >= release_at {
                    // The last delivery ends.
                    load.0.store(false, std::sync::atomic::Ordering::Release);
                }
            },
            AttestationIo {
                checkpoints: None,
                pacer: Some(&pacer),
            },
        )
        .await
        .expect("paced attestation");
        let samples = samples.into_inner().expect("samples");
        assert!(pacer.was_capped());
        assert!(!pacer.capped(), "the cap is off once nothing plays");

        // Every read issued while a delivery was live: up to and including
        // the one that ended where the last delivery ended.
        let capped = samples
            .iter()
            .position(|(_, bytes)| *bytes >= release_at)
            .expect("release sample")
            + 1;
        let window = Duration::from_secs(5);
        let allowance = rate * 5 + 2 * HASH_CHUNK as u64;
        for (index, (at, bytes)) in samples[..capped].iter().enumerate() {
            for (later_at, later_bytes) in &samples[index..capped] {
                if later_at.saturating_duration_since(*at) <= window {
                    assert!(
                        later_bytes - bytes <= allowance,
                        "{} bytes in {:?} exceeds the cap",
                        later_bytes - bytes,
                        later_at.saturating_duration_since(*at)
                    );
                }
            }
        }
        let released = samples
            .iter()
            .find(|(_, bytes)| *bytes >= release_at)
            .expect("release sample")
            .0;
        assert!(
            released.saturating_duration_since(started)
                >= Duration::from_secs_f64((release_at - HASH_CHUNK as u64) as f64 / rate as f64),
            "the capped stretch ran at the cap"
        );
        assert_eq!(
            samples.last().expect("final sample").0,
            released,
            "after the last delivery ends the rest of the file reads at full speed"
        );
        assert_eq!(attested.observation.source_sha256.len(), 64);
    }

    /// D-M2: a restart empties the in-memory table; the next attempt resumes
    /// from the node-local stored row, and the digest is the one an
    /// uninterrupted read produces.
    #[tokio::test]
    async fn a_restarted_attestation_resumes_from_the_stored_checkpoint() {
        let dir = tempfile::tempdir().expect("durable fixture");
        let path = dir.path().join("durable.bin");
        let size = 6 * FULL_DIGEST_CHECKPOINT_BYTES as usize + 4_321;
        tokio::fs::write(&path, filler(size))
            .await
            .expect("durable fixture");
        let file = hevc_test_file(4_102, path);
        let store = plurx_core::store::SqliteStore::open_in_memory().expect("store");
        let io = AttestationIo {
            checkpoints: Some(&store as &dyn Store),
            pacer: None,
        };
        let node = "durable-node";
        let past = 2 * FULL_DIGEST_CHECKPOINT_BYTES + HASH_CHUNK as u64;
        let stop = tokio::sync::Notify::new();
        let report = |bytes: u64| {
            if bytes >= past {
                stop.notify_one();
            }
        };
        tokio::select! {
            biased;
            () = stop.notified() => {}
            result = attest_copy_source_with(node, &file, None, &report, io) => panic!(
                "the attempt was meant to be preempted: {:?}",
                result.map(|attested| attested.observation.source_sha256)
            ),
        }
        let key = full_key(node, &file);
        assert!(full_digest_checkpoints().load(&key).is_some());
        // The daemon restarts: nothing in memory survives.
        full_digest_checkpoints().clear(&key);
        assert!(full_digest_checkpoints().load(&key).is_none());

        let first = std::sync::Mutex::new(None);
        let resumed = attest_copy_source_with(
            node,
            &file,
            None,
            &|bytes| {
                first.lock().expect("first").get_or_insert(bytes);
            },
            io,
        )
        .await
        .expect("resumed attestation");
        let resumed_from = first
            .into_inner()
            .expect("first")
            .expect("a progress report");
        assert!(
            resumed_from >= 2 * FULL_DIGEST_CHECKPOINT_BYTES && resumed_from < size as u64,
            "the restarted attempt resumes at the stored checkpoint, not byte 0: {resumed_from}"
        );
        let (_, reference) = first_progress_and_digest("durable-reference-node", &file).await;
        assert_eq!(resumed.observation.source_sha256, reference);
    }
}
