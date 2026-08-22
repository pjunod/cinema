//! Immutable object inventory for one published cache generation.
//!
//! Placement reads this small file once. Object bytes are verified at
//! publication, on the requested-object path, and by later scrubs; an offer
//! never walks an entire film just to prove that one node owns it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub const MANIFEST_FILE: &str = "generation-manifest.json";
const CHECKPOINT_FILE: &str = ".generation-manifest.checkpoint.json";
const FORMAT_VERSION: u16 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_OBJECTS: usize = 100_000;
/// One HLS object may consume at most this much publication, verification, or
/// scrub I/O. The cap prevents a corrupt manifest from turning a bounded
/// integrity pass into an unbounded disk read.
pub const MAX_OBJECT_BYTES: u64 = 128 * 1024 * 1024;
const RESPONSE_SNAPSHOT_MIB: u64 = 1024 * 1024;
const RESPONSE_SMALL_MAX_MIB: u64 = 8;
const RESPONSE_MEDIUM_MAX_MIB: u64 = 64;
const RESPONSE_SMALL_BUDGET_MIB: usize = 64;
const RESPONSE_MEDIUM_BUDGET_MIB: usize = 64;
const RESPONSE_LARGE_BUDGET_MIB: usize = 128;
static RESPONSE_SMALL_BUDGET: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
static RESPONSE_MEDIUM_BUDGET: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
static RESPONSE_LARGE_BUDGET: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
const CHECKPOINT_INTERVAL: usize = 32;
const MAX_CHECKPOINT_BYTES: u64 = MAX_MANIFEST_BYTES * 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GenerationObject {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GenerationManifest {
    pub format_version: u16,
    pub generation_id: String,
    pub object_count: usize,
    pub objects: Vec<GenerationObject>,
    pub manifest_digest: String,
}

pub struct VerifiedObject {
    pub file: tokio::fs::File,
    pub bytes: u64,
    pub lease: VerifiedObjectLease,
}

pub struct VerifiedObjectLease {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Serialize)]
struct ManifestBody<'a> {
    format_version: u16,
    generation_id: &'a str,
    object_count: usize,
    objects: &'a [GenerationObject],
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct FileFingerprint {
    bytes: u64,
    modified_secs: u64,
    modified_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_secs: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct CheckpointObject {
    object: GenerationObject,
    fingerprint: FileFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct CheckpointHeader {
    format_version: u16,
    generation_id: String,
}

fn safe_object_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && name != "."
        && name != ".."
}

async fn open_read_nofollow(path: &Path) -> std::io::Result<tokio::fs::File> {
    crate::fs_secure::open_read_nofollow(path).await
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| format!("{} has no safe filename", path.display()))?;
    crate::fs_secure::atomic_write_child(parent, name, bytes)
        .await
        .map_err(|error| format!("publishing {}: {error}", path.display()))
}

fn fingerprint(metadata: &std::fs::Metadata) -> FileFingerprint {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    FileFingerprint {
        bytes: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(unix)]
        changed_secs: {
            use std::os::unix::fs::MetadataExt;
            metadata.ctime()
        },
        #[cfg(unix)]
        changed_nanos: {
            use std::os::unix::fs::MetadataExt;
            metadata.ctime_nsec()
        },
    }
}

async fn object_digest_controlled<F>(
    path: &Path,
    should_yield: &mut F,
) -> Result<Option<(GenerationObject, FileFingerprint)>, String>
where
    F: FnMut() -> bool,
{
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "generation object {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_OBJECT_BYTES {
        return Err(format!(
            "generation object {} exceeds the {} byte bound",
            path.display(),
            MAX_OBJECT_BYTES
        ));
    }
    let mut file = open_read_nofollow(path)
        .await
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_OBJECT_BYTES {
        return Err(format!(
            "generation object {} is not a bounded regular file",
            path.display()
        ));
    }
    let opened_fingerprint = fingerprint(&opened_metadata);
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        if should_yield() {
            return Ok(None);
        }
        let read_limit = MAX_OBJECT_BYTES
            .saturating_sub(bytes)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..read_limit])
            .await
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        if bytes > MAX_OBJECT_BYTES {
            return Err(format!(
                "generation object {} grew beyond the {} byte bound while reading",
                path.display(),
                MAX_OBJECT_BYTES
            ));
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if fingerprint(&final_metadata) != opened_fingerprint || bytes != opened_fingerprint.bytes {
        return Err(format!(
            "generation object {} changed while hashing",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| format!("generation object {} has no safe name", path.display()))?;
    Ok(Some((
        GenerationObject {
            name: name.to_owned(),
            bytes,
            sha256: hex::encode(hasher.finalize()),
        },
        opened_fingerprint,
    )))
}

async fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > max_bytes
    {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let mut file = open_read_nofollow(path)
        .await
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    let opened = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !opened.is_file() || opened.len() > max_bytes {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    (&mut file)
        .take(opened.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    if bytes.len() as u64 != opened.len() {
        return Err(format!("{} changed while reading", path.display()));
    }
    Ok(bytes)
}

async fn read_bounded_file_controlled<F>(
    path: &Path,
    max_bytes: u64,
    should_yield: &mut F,
) -> Result<Option<Vec<u8>>, String>
where
    F: FnMut() -> bool,
{
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > max_bytes
    {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let mut file = open_read_nofollow(path)
        .await
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    let opened = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !opened.is_file() || opened.len() > max_bytes {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let mut encoded = Vec::with_capacity(opened.len() as usize);
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        if should_yield() {
            return Ok(None);
        }
        let remaining = opened
            .len()
            .saturating_sub(encoded.len() as u64)
            .saturating_add(1);
        let chunk = remaining.min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..chunk])
            .await
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        encoded.extend_from_slice(&buffer[..read]);
        if encoded.len() as u64 > opened.len() {
            return Err(format!("{} changed while reading", path.display()));
        }
    }
    Ok(Some(encoded))
}

/// Read one safe, regular generation object without trusting its pathname
/// size. Legacy cache rows have no manifest yet, but their first playlist
/// inspection must still be bounded before adoption.
pub async fn read_bounded_regular_object(
    root: &Path,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    if !safe_object_name(name) {
        return Ok(None);
    }
    match read_bounded_file(&root.join(name), MAX_OBJECT_BYTES).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(_) => Ok(None),
    }
}

/// Read a playlist under the much smaller manifest budget. Media objects may
/// be large enough to stream, but no text parser should ever inherit that
/// allocation ceiling from them.
pub async fn read_bounded_playlist(root: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    if !safe_object_name(name) {
        return Ok(None);
    }
    match read_bounded_file(&root.join(name), MAX_MANIFEST_BYTES).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(_) => Ok(None),
    }
}

/// Open one legacy object through a no-follow handle with a trusted bounded
/// length. Callers can stream this handle without allocating the entire media
/// segment; legacy generations have no digest until their adoption pass.
pub async fn open_bounded_regular_object(
    root: &Path,
    name: &str,
) -> Result<Option<(tokio::fs::File, u64)>, String> {
    if !safe_object_name(name) {
        return Ok(None);
    }
    let path = root.join(name);
    let mut file = match open_read_nofollow(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("opening {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_OBJECT_BYTES {
        return Ok(None);
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| format!("rewinding {}: {error}", path.display()))?;
    Ok(Some((file, metadata.len())))
}

enum CheckpointLoad {
    Ready {
        objects: Vec<CheckpointObject>,
        needs_repair: bool,
    },
    Yielded,
}

async fn load_checkpoint<F>(
    root: &Path,
    generation_id: &str,
    ordered_names: &[String],
    should_yield: &mut F,
) -> CheckpointLoad
where
    F: FnMut() -> bool,
{
    let path = root.join(CHECKPOINT_FILE);
    let encoded =
        match read_bounded_file_controlled(&path, MAX_CHECKPOINT_BYTES, should_yield).await {
            Ok(Some(encoded)) => encoded,
            Ok(None) => return CheckpointLoad::Yielded,
            Err(_) => {
                return CheckpointLoad::Ready {
                    objects: Vec::new(),
                    needs_repair: true,
                }
            }
        };
    let mut lines = encoded.split_inclusive(|byte| *byte == b'\n');
    let Some(header_record) = lines.next().filter(|line| !line.is_empty()) else {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    };
    let header_complete = header_record.ends_with(b"\n");
    let header_line = header_record.strip_suffix(b"\n").unwrap_or(header_record);
    let Ok(header) = serde_json::from_slice::<CheckpointHeader>(header_line) else {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    };
    if !header_complete
        || header.format_version != FORMAT_VERSION
        || header.generation_id != generation_id
    {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    }
    let mut objects = Vec::new();
    let mut needs_repair = false;
    for record in lines.filter(|line| !line.is_empty()) {
        if should_yield() {
            return CheckpointLoad::Yielded;
        }
        let complete = record.ends_with(b"\n");
        let line = record.strip_suffix(b"\n").unwrap_or(record);
        if line.is_empty() {
            continue;
        }
        let Ok(completed) = serde_json::from_slice::<CheckpointObject>(line) else {
            needs_repair = true;
            break;
        };
        if !complete {
            needs_repair = true;
        }
        let index = objects.len();
        if index >= ordered_names.len() {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        if completed.object.name != ordered_names[index]
            || completed.object.bytes > MAX_OBJECT_BYTES
            || completed.object.sha256.len() != 64
            || !completed
                .object
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        let object_path = root.join(&completed.object.name);
        let Ok(metadata) = tokio::fs::symlink_metadata(&object_path).await else {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || fingerprint(&metadata) != completed.fingerprint
        {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        objects.push(completed);
    }
    CheckpointLoad::Ready {
        objects,
        needs_repair,
    }
}

/// Append only newly completed objects. The checkpoint is an optimization:
/// reaching its own conservative cap disables further persistence but can
/// never make an otherwise valid final manifest fail.
async fn append_checkpoint(
    root: &Path,
    generation_id: &str,
    objects: &[CheckpointObject],
) -> Result<bool, String> {
    let path = root.join(CHECKPOINT_FILE);
    let mut encoded = Vec::new();
    for object in objects {
        serde_json::to_writer(&mut encoded, object)
            .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
        encoded.push(b'\n');
    }
    let mut existing = match open_read_nofollow(&path).await {
        Ok(mut file) => {
            let metadata = file
                .metadata()
                .await
                .map_err(|error| format!("reading generation checkpoint metadata: {error}"))?;
            if !metadata.is_file() || metadata.len() > MAX_CHECKPOINT_BYTES {
                return Err("generation checkpoint is not a bounded regular file".to_owned());
            }
            let mut existing = Vec::with_capacity(metadata.len() as usize);
            (&mut file)
                .take(metadata.len().saturating_add(1))
                .read_to_end(&mut existing)
                .await
                .map_err(|error| format!("reading generation checkpoint: {error}"))?;
            if existing.len() as u64 != metadata.len() {
                return Err("generation checkpoint changed while reading".to_owned());
            }
            existing
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let header = CheckpointHeader {
                format_version: FORMAT_VERSION,
                generation_id: generation_id.to_owned(),
            };
            let mut initial = serde_json::to_vec(&header)
                .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
            initial.push(b'\n');
            initial
        }
        Err(error) => return Err(format!("opening generation checkpoint: {error}")),
    };
    if (existing.len() as u64).saturating_add(encoded.len() as u64) > MAX_CHECKPOINT_BYTES {
        return Ok(false);
    }
    existing.extend_from_slice(&encoded);
    atomic_write(&path, &existing).await?;
    Ok(true)
}

async fn replace_checkpoint(
    root: &Path,
    generation_id: &str,
    objects: &[CheckpointObject],
) -> Result<(), String> {
    let checkpoint = CheckpointHeader {
        format_version: FORMAT_VERSION,
        generation_id: generation_id.to_owned(),
    };
    let mut encoded = serde_json::to_vec(&checkpoint)
        .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
    encoded.push(b'\n');
    for object in objects {
        serde_json::to_writer(&mut encoded, object)
            .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
        encoded.push(b'\n');
    }
    if encoded.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err("generation checkpoint exceeds its size bound".to_owned());
    }
    let path = root.join(CHECKPOINT_FILE);
    atomic_write(&path, &encoded).await
}

fn body_digest(generation_id: &str, objects: &[GenerationObject]) -> Result<String, String> {
    let body = ManifestBody {
        format_version: FORMAT_VERSION,
        generation_id,
        object_count: objects.len(),
        objects,
    };
    let encoded = serde_json::to_vec(&body)
        .map_err(|error| format!("serializing generation manifest: {error}"))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

/// Hash every named object once and publish the immutable manifest beside it.
pub async fn publish(
    root: &Path,
    generation_id: &str,
    ordered_names: &[String],
) -> Result<GenerationManifest, String> {
    publish_controlled(root, generation_id, ordered_names, || false)
        .await?
        .ok_or_else(|| "uncontrolled manifest publication yielded".to_owned())
}

/// Hash and publish one immutable generation while remaining preemptible.
///
/// Completed object digests are checkpointed beside the staging generation.
/// A retry reuses a digest only while the file's device/inode, size and mtime
/// still match, so foreground work or a lost queue fence can stop hashing at
/// chunk granularity without restarting the whole title later.
pub async fn publish_controlled<F>(
    root: &Path,
    generation_id: &str,
    ordered_names: &[String],
    mut should_yield: F,
) -> Result<Option<GenerationManifest>, String>
where
    F: FnMut() -> bool,
{
    if generation_id.is_empty()
        || generation_id.len() > 256
        || ordered_names.is_empty()
        || ordered_names.len() > MAX_OBJECTS
        || ordered_names.iter().any(|name| !safe_object_name(name))
        || ordered_names.iter().collect::<BTreeSet<_>>().len() != ordered_names.len()
    {
        return Err("invalid generation manifest identity or object list".to_owned());
    }
    let (mut completed, checkpoint_needs_repair) =
        match load_checkpoint(root, generation_id, ordered_names, &mut should_yield).await {
            CheckpointLoad::Ready {
                objects,
                needs_repair,
            } => (objects, needs_repair),
            CheckpointLoad::Yielded => return Ok(None),
        };
    let mut checkpoint_enabled = true;
    if completed.is_empty() || checkpoint_needs_repair {
        // A stale/corrupt checkpoint is never extended under a new identity.
        // Persistence is best effort; hashing and final publication remain
        // correct even on a read-only/full staging filesystem.
        if let Err(error) = replace_checkpoint(root, generation_id, &completed).await {
            tracing::warn!(%error, "generation digest checkpoint disabled");
            checkpoint_enabled = false;
        }
    }
    let mut persisted = completed.len();
    for name in ordered_names.iter().skip(completed.len()) {
        if should_yield() {
            if checkpoint_enabled && persisted < completed.len() {
                if let Err(error) =
                    append_checkpoint(root, generation_id, &completed[persisted..]).await
                {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                }
            }
            return Ok(None);
        }
        let Some((object, fingerprint)) =
            object_digest_controlled(&root.join(name), &mut should_yield).await?
        else {
            if checkpoint_enabled && persisted < completed.len() {
                if let Err(error) =
                    append_checkpoint(root, generation_id, &completed[persisted..]).await
                {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                }
            }
            return Ok(None);
        };
        completed.push(CheckpointObject {
            object,
            fingerprint,
        });
        if checkpoint_enabled && completed.len() - persisted >= CHECKPOINT_INTERVAL {
            match append_checkpoint(root, generation_id, &completed[persisted..]).await {
                Ok(true) => persisted = completed.len(),
                Ok(false) => checkpoint_enabled = false,
                Err(error) => {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                    checkpoint_enabled = false;
                }
            }
        }
    }
    if should_yield() {
        if checkpoint_enabled && persisted < completed.len() {
            if let Err(error) =
                append_checkpoint(root, generation_id, &completed[persisted..]).await
            {
                tracing::warn!(%error, "generation digest checkpoint disabled");
            }
        }
        return Ok(None);
    }
    let mut objects = completed
        .into_iter()
        .map(|completed| completed.object)
        .collect::<Vec<_>>();
    objects.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    if objects
        .iter()
        .any(|object| object.name == "index.m3u8" && object.bytes > MAX_MANIFEST_BYTES)
    {
        return Err("generation playlist exceeds its size bound".to_owned());
    }
    let manifest = GenerationManifest {
        format_version: FORMAT_VERSION,
        generation_id: generation_id.to_owned(),
        object_count: objects.len(),
        manifest_digest: body_digest(generation_id, &objects)?,
        objects,
    };
    let encoded = serde_json::to_vec(&manifest)
        .map_err(|error| format!("serializing generation manifest: {error}"))?;
    if encoded.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("generation manifest exceeds its size bound".to_owned());
    }
    let path = root.join(MANIFEST_FILE);
    atomic_write(&path, &encoded).await?;
    match tokio::fs::remove_file(root.join(CHECKPOINT_FILE)).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("removing generation checkpoint: {error}")),
    }
    Ok(Some(manifest))
}

async fn load_checkpoint_directory<F>(
    root: &crate::fs_secure::SecureDirectory,
    generation_id: &str,
    ordered_names: &[String],
    should_yield: &mut F,
) -> CheckpointLoad
where
    F: FnMut() -> bool,
{
    let encoded = match root
        .read_bounded_child(CHECKPOINT_FILE, MAX_CHECKPOINT_BYTES)
        .await
    {
        Ok(encoded) => encoded,
        Err(_) => {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
    };
    let mut lines = encoded.split_inclusive(|byte| *byte == b'\n');
    let Some(header_record) = lines.next().filter(|line| !line.is_empty()) else {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    };
    let header_complete = header_record.ends_with(b"\n");
    let header_line = header_record.strip_suffix(b"\n").unwrap_or(header_record);
    let Ok(header) = serde_json::from_slice::<CheckpointHeader>(header_line) else {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    };
    if !header_complete
        || header.format_version != FORMAT_VERSION
        || header.generation_id != generation_id
    {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    }
    let mut objects = Vec::new();
    let mut needs_repair = false;
    for record in lines.filter(|line| !line.is_empty()) {
        if should_yield() {
            return CheckpointLoad::Yielded;
        }
        let complete = record.ends_with(b"\n");
        let line = record.strip_suffix(b"\n").unwrap_or(record);
        if line.is_empty() {
            continue;
        }
        let Ok(completed) = serde_json::from_slice::<CheckpointObject>(line) else {
            needs_repair = true;
            break;
        };
        if !complete {
            needs_repair = true;
        }
        let index = objects.len();
        if index >= ordered_names.len()
            || completed.object.name != ordered_names[index]
            || completed.object.bytes > MAX_OBJECT_BYTES
            || completed.object.sha256.len() != 64
            || !completed
                .object
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        let Ok(file) = root.open_read_child(&completed.object.name).await else {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        };
        let Ok(metadata) = file.metadata().await else {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        };
        if !metadata.is_file() || fingerprint(&metadata) != completed.fingerprint {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        objects.push(completed);
    }
    CheckpointLoad::Ready {
        objects,
        needs_repair,
    }
}

async fn replace_checkpoint_directory(
    root: &crate::fs_secure::SecureDirectory,
    generation_id: &str,
    objects: &[CheckpointObject],
) -> Result<(), String> {
    let checkpoint = CheckpointHeader {
        format_version: FORMAT_VERSION,
        generation_id: generation_id.to_owned(),
    };
    let mut encoded = serde_json::to_vec(&checkpoint)
        .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
    encoded.push(b'\n');
    for object in objects {
        serde_json::to_writer(&mut encoded, object)
            .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
        encoded.push(b'\n');
    }
    if encoded.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err("generation checkpoint exceeds its size bound".to_owned());
    }
    root.atomic_write_child(CHECKPOINT_FILE, &encoded)
        .await
        .map_err(|error| format!("publishing generation checkpoint: {error}"))
}

async fn append_checkpoint_directory(
    root: &crate::fs_secure::SecureDirectory,
    generation_id: &str,
    objects: &[CheckpointObject],
) -> Result<bool, String> {
    let mut existing = match root
        .read_bounded_child(CHECKPOINT_FILE, MAX_CHECKPOINT_BYTES)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let header = CheckpointHeader {
                format_version: FORMAT_VERSION,
                generation_id: generation_id.to_owned(),
            };
            let mut bytes = serde_json::to_vec(&header)
                .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
            bytes.push(b'\n');
            bytes
        }
        Err(error) => return Err(format!("reading generation checkpoint: {error}")),
    };
    for object in objects {
        serde_json::to_writer(&mut existing, object)
            .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
        existing.push(b'\n');
    }
    if existing.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Ok(false);
    }
    root.atomic_write_child(CHECKPOINT_FILE, &existing)
        .await
        .map_err(|error| format!("publishing generation checkpoint: {error}"))?;
    Ok(true)
}

async fn object_digest_directory<F>(
    root: &crate::fs_secure::SecureDirectory,
    name: &str,
    should_yield: &mut F,
) -> Result<Option<(GenerationObject, FileFingerprint)>, String>
where
    F: FnMut() -> bool,
{
    let mut file = root
        .open_read_child(name)
        .await
        .map_err(|error| format!("opening generation object {name}: {error}"))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| format!("reading generation object {name} metadata: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_OBJECT_BYTES {
        return Err(format!(
            "generation object {name} exceeds its regular-file bound"
        ));
    }
    let opened_fingerprint = fingerprint(&metadata);
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        if should_yield() {
            return Ok(None);
        }
        let read_limit = MAX_OBJECT_BYTES
            .saturating_sub(bytes)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..read_limit])
            .await
            .map_err(|error| format!("reading generation object {name}: {error}"))?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        if bytes > MAX_OBJECT_BYTES {
            return Err(format!("generation object {name} grew beyond its bound"));
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file
        .metadata()
        .await
        .map_err(|error| format!("remeasuring generation object {name}: {error}"))?;
    if fingerprint(&final_metadata) != opened_fingerprint || bytes != opened_fingerprint.bytes {
        return Err(format!("generation object {name} changed while hashing"));
    }
    Ok(Some((
        GenerationObject {
            name: name.to_owned(),
            bytes,
            sha256: hex::encode(hasher.finalize()),
        },
        opened_fingerprint,
    )))
}

/// Capability-relative variant used by the producer staging pipeline. It
/// retains the same resumable checkpoints as pathname publication while no
/// intermediate staging component can be redirected after binding.
pub async fn publish_controlled_directory<F>(
    root: &crate::fs_secure::SecureDirectory,
    generation_id: &str,
    ordered_names: &[String],
    mut should_yield: F,
) -> Result<Option<GenerationManifest>, String>
where
    F: FnMut() -> bool,
{
    if generation_id.is_empty()
        || generation_id.len() > 256
        || ordered_names.is_empty()
        || ordered_names.len() > MAX_OBJECTS
        || ordered_names.iter().any(|name| !safe_object_name(name))
        || ordered_names.iter().collect::<BTreeSet<_>>().len() != ordered_names.len()
    {
        return Err("invalid generation manifest identity or object list".to_owned());
    }
    let (mut completed, checkpoint_needs_repair) = match load_checkpoint_directory(
        root,
        generation_id,
        ordered_names,
        &mut should_yield,
    )
    .await
    {
        CheckpointLoad::Ready {
            objects,
            needs_repair,
        } => (objects, needs_repair),
        CheckpointLoad::Yielded => return Ok(None),
    };
    let mut checkpoint_enabled = true;
    if completed.is_empty() || checkpoint_needs_repair {
        if let Err(error) = replace_checkpoint_directory(root, generation_id, &completed).await {
            tracing::warn!(%error, "generation digest checkpoint disabled");
            checkpoint_enabled = false;
        }
    }
    let mut persisted = completed.len();
    for name in ordered_names.iter().skip(completed.len()) {
        if should_yield() {
            if checkpoint_enabled && persisted < completed.len() {
                if let Err(error) =
                    append_checkpoint_directory(root, generation_id, &completed[persisted..]).await
                {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                }
            }
            return Ok(None);
        }
        let Some((object, fingerprint)) =
            object_digest_directory(root, name, &mut should_yield).await?
        else {
            if checkpoint_enabled && persisted < completed.len() {
                if let Err(error) =
                    append_checkpoint_directory(root, generation_id, &completed[persisted..]).await
                {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                }
            }
            return Ok(None);
        };
        completed.push(CheckpointObject {
            object,
            fingerprint,
        });
        if checkpoint_enabled && completed.len() - persisted >= CHECKPOINT_INTERVAL {
            match append_checkpoint_directory(root, generation_id, &completed[persisted..]).await {
                Ok(true) => persisted = completed.len(),
                Ok(false) => checkpoint_enabled = false,
                Err(error) => {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                    checkpoint_enabled = false;
                }
            }
        }
    }
    if should_yield() {
        if checkpoint_enabled && persisted < completed.len() {
            let _ = append_checkpoint_directory(root, generation_id, &completed[persisted..]).await;
        }
        return Ok(None);
    }
    let mut objects = completed
        .into_iter()
        .map(|completed| completed.object)
        .collect::<Vec<_>>();
    objects.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    if objects
        .iter()
        .any(|object| object.name == "index.m3u8" && object.bytes > MAX_MANIFEST_BYTES)
    {
        return Err("generation playlist exceeds its size bound".to_owned());
    }
    let manifest = GenerationManifest {
        format_version: FORMAT_VERSION,
        generation_id: generation_id.to_owned(),
        object_count: objects.len(),
        manifest_digest: body_digest(generation_id, &objects)?,
        objects,
    };
    let encoded = serde_json::to_vec(&manifest)
        .map_err(|error| format!("serializing generation manifest: {error}"))?;
    if encoded.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("generation manifest exceeds its size bound".to_owned());
    }
    root.atomic_write_child(MANIFEST_FILE, &encoded)
        .await
        .map_err(|error| format!("publishing generation manifest: {error}"))?;
    match root.unlink_child(CHECKPOINT_FILE).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("removing generation checkpoint: {error}")),
    }
    Ok(Some(manifest))
}

fn parse_manifest(encoded: &[u8]) -> Result<GenerationManifest, String> {
    let manifest: GenerationManifest = serde_json::from_slice(encoded)
        .map_err(|error| format!("parsing generation manifest: {error}"))?;
    if manifest.format_version != FORMAT_VERSION
        || manifest.generation_id.is_empty()
        || manifest.generation_id.len() > 256
        || manifest.object_count != manifest.objects.len()
        || manifest.objects.is_empty()
        || manifest.objects.len() > MAX_OBJECTS
        || manifest.objects.iter().any(|object| {
            !safe_object_name(&object.name)
                || object.bytes > MAX_OBJECT_BYTES
                || (object.name == "index.m3u8" && object.bytes > MAX_MANIFEST_BYTES)
                || object.sha256.len() != 64
                || !object.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || manifest
            .objects
            .windows(2)
            .any(|pair| pair[0].name >= pair[1].name)
        || manifest
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != manifest.objects.len()
        || manifest.manifest_digest.len() != 64
        || !manifest
            .manifest_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || body_digest(&manifest.generation_id, &manifest.objects)? != manifest.manifest_digest
    {
        return Err("generation manifest failed validation".to_owned());
    }
    Ok(manifest)
}

/// Load and authenticate the small manifest from one open handle.
///
/// `read_budget` is charged conservatively as the opened length plus one byte
/// used to detect concurrent growth. `None` means the caller should defer this
/// location without reading it; malformed or replaced bytes remain an error.
pub async fn load_with_budget(
    root: &Path,
    read_budget: u64,
) -> Result<Option<(GenerationManifest, u64)>, String> {
    let path = root.join(MANIFEST_FILE);
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("generation manifest is not a bounded regular file".to_owned());
    }
    let mut file = open_read_nofollow(&path)
        .await
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .await
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_MANIFEST_BYTES {
        return Err("generation manifest is not a bounded regular file".to_owned());
    }
    let charged_bytes = opened_metadata.len().saturating_add(1);
    if charged_bytes > read_budget {
        return Ok(None);
    }
    let mut encoded = Vec::with_capacity(opened_metadata.len() as usize);
    (&mut file)
        .take(charged_bytes)
        .read_to_end(&mut encoded)
        .await
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    if encoded.len() as u64 > opened_metadata.len() {
        return Err("generation manifest grew beyond its size bound while reading".to_owned());
    }
    Ok(Some((parse_manifest(&encoded)?, charged_bytes)))
}

/// Load and authenticate the small manifest without touching media objects.
pub async fn load(root: &Path) -> Result<GenerationManifest, String> {
    load_with_budget(root, MAX_MANIFEST_BYTES + 1)
        .await?
        .map(|(manifest, _)| manifest)
        .ok_or_else(|| "generation manifest exceeds its read budget".to_owned())
}

impl GenerationManifest {
    fn object(&self, name: &str) -> Option<&GenerationObject> {
        self.objects
            .binary_search_by(|object| object.name.as_str().cmp(name))
            .ok()
            .map(|index| &self.objects[index])
    }

    pub fn contains_object(&self, name: &str) -> bool {
        self.object(name).is_some()
    }

    /// Authenticate bytes already read for a response. Reopening the pathname
    /// here would verify a different object if a rename lands between the
    /// response read and the integrity check.
    pub fn verify_bytes(&self, name: &str, bytes: &[u8]) -> bool {
        self.object(name).is_some_and(|expected| {
            bytes.len() as u64 == expected.bytes
                && hex::encode(Sha256::digest(bytes)) == expected.sha256
        })
    }

    /// Copy one object into an unlinked descriptor while hashing it, then
    /// rewind and return that immutable-by-capability snapshot. A pathname
    /// replacement or in-place mutation of the source inode after validation
    /// therefore cannot change the bytes the HTTP response streams.
    pub async fn open_verified_object(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<Option<VerifiedObject>, String> {
        let Some(expected) = self.object(name) else {
            return Ok(None);
        };
        let path = root.join(name);
        let path_metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
        if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
            return Ok(None);
        }
        let mut file = open_read_nofollow(&path)
            .await
            .map_err(|error| format!("opening {}: {error}", path.display()))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_OBJECT_BYTES {
            return Ok(None);
        }
        let units = expected.bytes.div_ceil(RESPONSE_SNAPSHOT_MIB).max(1) as u32;
        // Independent size classes keep a single 128-MiB waiter from sitting
        // at the head of Tokio's fair weighted semaphore and blocking every
        // tiny HLS segment behind it. Their fixed budgets still sum to the
        // same hard 256-MiB process ceiling.
        let budget = if units as u64 <= RESPONSE_SMALL_MAX_MIB {
            RESPONSE_SMALL_BUDGET
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_SMALL_BUDGET_MIB)))
        } else if units as u64 <= RESPONSE_MEDIUM_MAX_MIB {
            RESPONSE_MEDIUM_BUDGET
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_MEDIUM_BUDGET_MIB)))
        } else {
            RESPONSE_LARGE_BUDGET
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_LARGE_BUDGET_MIB)))
        }
        .clone();
        let permit = budget
            .acquire_many_owned(units)
            .await
            .map_err(|_| "authenticated response snapshot budget closed".to_owned())?;
        let mut hasher = Sha256::new();
        let mut snapshot = crate::fs_secure::anonymous_memory_file()
            .map_err(|error| format!("creating authenticated memory snapshot: {error}"))?;
        let mut bytes = 0_u64;
        let mut buffer = vec![0_u8; 128 * 1024];
        loop {
            let read_limit = expected
                .bytes
                .saturating_sub(bytes)
                .saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let read = file
                .read(&mut buffer[..read_limit])
                .await
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            bytes = bytes.saturating_add(read as u64);
            if bytes > expected.bytes {
                return Ok(None);
            }
            hasher.update(&buffer[..read]);
            snapshot
                .write_all(&buffer[..read])
                .await
                .map_err(|error| format!("snapshotting {}: {error}", path.display()))?;
        }
        if bytes != expected.bytes || hex::encode(hasher.finalize()) != expected.sha256 {
            return Ok(None);
        }
        crate::fs_secure::seal_anonymous_memory_file(&snapshot)
            .map_err(|error| format!("sealing authenticated memory snapshot: {error}"))?;
        snapshot
            .seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|error| format!("rewinding authenticated snapshot: {error}"))?;
        Ok(Some(VerifiedObject {
            file: snapshot,
            bytes: expected.bytes,
            lease: VerifiedObjectLease { _permit: permit },
        }))
    }

    /// Read one authenticated object from the same bounded file handle used
    /// for verification. This is for endpoints that need owned bytes (small
    /// playlists and offline responses); streaming endpoints should retain
    /// the handle returned by [`Self::open_verified_object`].
    pub async fn read_verified_object(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(expected) = self.object(name) else {
            return Ok(None);
        };
        let path = root.join(name);
        let path_metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
        if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
            return Ok(None);
        }
        let mut file = open_read_nofollow(&path)
            .await
            .map_err(|error| format!("opening {}: {error}", path.display()))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
        if !metadata.is_file()
            || metadata.len() != expected.bytes
            || metadata.len() > MAX_OBJECT_BYTES
        {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(expected.bytes as usize);
        (&mut file)
            .take(expected.bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("reading verified generation object {name}: {error}"))?;
        Ok(self.verify_bytes(name, &bytes).then_some(bytes))
    }

    /// Read and authenticate playlist text without inheriting the media-object
    /// allocation ceiling.
    pub async fn read_verified_playlist(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        if self
            .object(name)
            .is_none_or(|object| object.bytes > MAX_MANIFEST_BYTES)
        {
            return Ok(None);
        }
        self.read_verified_object(root, name).await
    }

    /// Verify only the object a reader is about to serve.
    pub async fn verify_object(&self, root: &Path, name: &str) -> Result<bool, String> {
        let Some(expected) = self.object(name) else {
            return Ok(false);
        };
        let path = root.join(name);
        let mut file = open_read_nofollow(&path)
            .await
            .map_err(|error| format!("opening {}: {error}", path.display()))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
        if !metadata.is_file()
            || metadata.len() != expected.bytes
            || metadata.len() > MAX_OBJECT_BYTES
        {
            return Ok(false);
        }
        let mut hasher = Sha256::new();
        let mut read_bytes = 0_u64;
        let mut buffer = vec![0_u8; 128 * 1024];
        loop {
            let read_limit = expected
                .bytes
                .saturating_sub(read_bytes)
                .saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let read = file
                .read(&mut buffer[..read_limit])
                .await
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            read_bytes = read_bytes.saturating_add(read as u64);
            if read_bytes > expected.bytes {
                return Ok(false);
            }
            hasher.update(&buffer[..read]);
        }
        Ok(read_bytes == expected.bytes && hex::encode(hasher.finalize()) == expected.sha256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requested_object_verification_detects_corruption_without_a_manifest_walk() {
        let directory = tempfile::tempdir().expect("generation directory");
        tokio::fs::write(
            directory.path().join("index.m3u8"),
            b"#EXTM3U\nseg00000.ts\n",
        )
        .await
        .expect("playlist");
        tokio::fs::write(directory.path().join("seg00000.ts"), b"original-segment")
            .await
            .expect("segment");

        publish(
            directory.path(),
            "job-1:fence-2",
            &["index.m3u8".to_owned(), "seg00000.ts".to_owned()],
        )
        .await
        .expect("publish manifest");
        let manifest = load(directory.path()).await.expect("load manifest");
        assert!(manifest
            .verify_object(directory.path(), "seg00000.ts")
            .await
            .expect("verify original segment"));

        tokio::fs::write(directory.path().join("seg00000.ts"), b"corrupted-segment")
            .await
            .expect("corrupt requested segment");
        let loaded_without_object_walk = load(directory.path())
            .await
            .expect("the manifest remains independently authentic");
        assert!(!loaded_without_object_walk
            .verify_object(directory.path(), "seg00000.ts")
            .await
            .expect("verify corrupt requested segment"));
        assert!(!loaded_without_object_walk
            .verify_object(directory.path(), "not-in-generation.ts")
            .await
            .expect("reject an unlisted object"));
    }

    #[tokio::test]
    async fn budgeted_manifest_load_refuses_before_reading_past_its_allowance() {
        let directory = tempfile::tempdir().expect("generation directory");
        tokio::fs::write(directory.path().join("index.m3u8"), b"#EXTM3U\n")
            .await
            .expect("playlist");
        publish(directory.path(), "generation-a", &["index.m3u8".to_owned()])
            .await
            .expect("publish manifest");
        let bytes = tokio::fs::metadata(directory.path().join(MANIFEST_FILE))
            .await
            .expect("manifest metadata")
            .len();

        assert!(
            load_with_budget(directory.path(), bytes)
                .await
                .expect("bounded load")
                .is_none(),
            "the load must reserve its one-byte growth probe before reading"
        );
        let (_, charged) = load_with_budget(directory.path(), bytes + 1)
            .await
            .expect("bounded load")
            .expect("sufficient read allowance");
        assert_eq!(charged, bytes + 1);
    }

    #[tokio::test]
    async fn playlist_authenticates_the_response_buffer_not_a_reopened_path() {
        let directory = tempfile::tempdir().expect("generation directory");
        let path = directory.path().join("index.m3u8");
        tokio::fs::write(&path, b"#EXTM3U\n#EXT-X-ENDLIST\n")
            .await
            .expect("playlist");
        let manifest = publish(directory.path(), "generation-a", &["index.m3u8".to_owned()])
            .await
            .expect("publish manifest");
        let response = tokio::fs::read(&path).await.expect("read response");
        tokio::fs::write(&path, b"corrupt replacement")
            .await
            .expect("replace pathname after response read");

        assert!(manifest.verify_bytes("index.m3u8", &response));
        assert!(!manifest.verify_bytes(
            "index.m3u8",
            &tokio::fs::read(&path).await.expect("replacement")
        ));
    }

    #[tokio::test]
    async fn segment_streams_the_same_handle_that_was_authenticated() {
        let directory = tempfile::tempdir().expect("generation directory");
        let path = directory.path().join("seg00000.ts");
        tokio::fs::write(&path, b"published segment")
            .await
            .expect("segment");
        let manifest = publish(
            directory.path(),
            "generation-a",
            &["seg00000.ts".to_owned()],
        )
        .await
        .expect("publish manifest");
        let mut verified = manifest
            .open_verified_object(directory.path(), "seg00000.ts")
            .await
            .expect("verify segment")
            .expect("valid segment");
        assert_eq!(verified.bytes, b"published segment".len() as u64);
        let replacement = directory.path().join("replacement.ts");
        tokio::fs::write(&replacement, b"corrupt replacement")
            .await
            .expect("replacement");
        tokio::fs::rename(&replacement, &path)
            .await
            .expect("replace pathname after verification");
        let mut served = Vec::new();
        verified
            .file
            .read_to_end(&mut served)
            .await
            .expect("read authenticated handle");

        assert_eq!(served, b"published segment");
        assert_eq!(
            tokio::fs::read(&path).await.expect("replacement bytes"),
            b"corrupt replacement"
        );
    }

    #[tokio::test]
    async fn manifest_rejects_duplicate_and_traversal_object_names() {
        let directory = tempfile::tempdir().expect("generation directory");
        tokio::fs::write(directory.path().join("seg00000.ts"), b"segment")
            .await
            .expect("segment");
        assert!(publish(
            directory.path(),
            "job-1:fence-1",
            &["seg00000.ts".to_owned(), "seg00000.ts".to_owned()],
        )
        .await
        .is_err());
        assert!(publish(
            directory.path(),
            "job-1:fence-1",
            &["../seg00000.ts".to_owned()],
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn publication_rejects_an_object_larger_than_the_scrub_io_ceiling() {
        let directory = tempfile::tempdir().expect("generation directory");
        let path = directory.path().join("seg00000.ts");
        let file = std::fs::File::create(&path).expect("sparse segment");
        file.set_len(MAX_OBJECT_BYTES + 1)
            .expect("extend sparse segment");

        assert!(publish(
            directory.path(),
            "generation-too-large",
            &["seg00000.ts".to_owned()],
        )
        .await
        .expect_err("oversized generation object")
        .contains("exceeds"));
    }

    #[tokio::test]
    async fn capability_publication_checkpoints_sub_interval_progress_before_yielding() {
        let directory = tempfile::tempdir().expect("generation directory");
        let names = (0..3)
            .map(|index| format!("seg{index:05}.ts"))
            .collect::<Vec<_>>();
        for name in &names {
            tokio::fs::write(directory.path().join(name), name.as_bytes())
                .await
                .expect("segment");
        }
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let mut calls = 0usize;
        let yielded = publish_controlled_directory(&capability, "generation-a", &names, || {
            calls += 1;
            calls >= 7
        })
        .await
        .expect("yielded publication");
        assert!(yielded.is_none());
        let checkpoint = capability
            .read_bounded_child(CHECKPOINT_FILE, MAX_CHECKPOINT_BYTES)
            .await
            .expect("durable checkpoint");
        assert_eq!(
            checkpoint
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            3,
            "header plus both completed objects are persisted below the 32-object interval"
        );

        let mut resume_calls = 0usize;
        let manifest = publish_controlled_directory(&capability, "generation-a", &names, || {
            resume_calls += 1;
            false
        })
        .await
        .expect("resumed publication")
        .expect("completed manifest");
        assert_eq!(manifest.object_count, 3);
        assert!(
            resume_calls <= 6,
            "resume should validate two checkpoint records and hash only the final object; got {resume_calls} yield probes"
        );
    }
}
