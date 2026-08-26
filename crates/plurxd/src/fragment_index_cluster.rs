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

/// Reconcile a bounded slice of node-local bytes with the authoritative
/// catalog. Any catalog read failure aborts the sweep and preserves data.
pub(crate) async fn sweep_local_orphans(
    store: &dyn Store,
    root: &Path,
    limit: usize,
) -> Result<usize, String> {
    let mut removed = 0_usize;
    let mut examined = 0_usize;
    let mut directories = match tokio::fs::read_dir(root).await {
        Ok(directories) => directories,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("list {}: {error}", root.display())),
    };
    while let Some(directory) = directories
        .next_entry()
        .await
        .map_err(|error| format!("walk {}: {error}", root.display()))?
    {
        if !directory
            .file_type()
            .await
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let mut files = tokio::fs::read_dir(directory.path())
            .await
            .map_err(|error| error.to_string())?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            if examined >= limit {
                return Ok(removed);
            }
            let name = file.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(cache_key) = name.strip_suffix(".idx") else {
                continue;
            };
            if !valid_digest(cache_key) {
                continue;
            }
            examined += 1;
            match store
                .cluster_fragment_index_artifact(cache_key)
                .await
                .map_err(|error| error.to_string())?
            {
                Some(_) => {}
                None => {
                    if remove_local_blob(root, cache_key).await {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
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

pub(crate) async fn attest_source(
    node_id: &str,
    file: &MediaFile,
    memo: Option<&FragmentIndexSourceObservation>,
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
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; HASH_CHUNK];
        loop {
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|error| format!("hashing {}: {error}", file.path.display()))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let after = source
            .metadata()
            .await
            .map_err(|error| format!("re-fstat {}: {error}", file.path.display()))?;
        scanner_identity_matches(&after, file)?;
        if object_version(&after)? != version {
            return Err("source changed while its complete digest was read".to_owned());
        }
        hex::encode(digest.finalize())
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

pub(crate) fn pipeline_digest(file: &MediaFile, engine_sha256: &str, have_dovi: bool) -> String {
    let mut args = plurx_core::transcode::copy_index_pipe_args(file, have_dovi, false);
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
}
