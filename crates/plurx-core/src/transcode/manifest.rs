//! Immutable object inventory for one published cache generation.
//!
//! Placement reads this small file once. Object bytes are verified at
//! publication, on the requested-object path, and by later scrubs; an offer
//! never walks an entire film just to prove that one node owns it.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub const MANIFEST_FILE: &str = "generation-manifest.json";
const CHECKPOINT_FILE: &str = ".generation-manifest.checkpoint.json";
const FORMAT_VERSION: u16 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum inventory guaranteed to serialize under [`MAX_MANIFEST_BYTES`]
/// even when every safe object name occupies its full 128-byte allowance and
/// every JSON character needs its permitted escaping. This also bounds the
/// final descriptor-relative fingerprint pass before publication.
pub const MAX_OBJECTS: usize = 8_192;
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
/// A checkpoint record is bounded independently of input: object names are at
/// most 128 bytes, both digests are fixed at 64 hex bytes, and every numeric
/// fingerprint field has a fixed-width integer representation. Keep a
/// deliberately conservative per-record allowance so every permitted object
/// can be checkpointed; reaching the ceiling must never make a yieldable
/// publication livelock on an unpersistable suffix.
const MAX_CHECKPOINT_RECORD_BYTES: u64 = 768;
const MAX_CHECKPOINT_HEADER_BYTES: u64 = 1_024;
const MAX_CHECKPOINT_BYTES: u64 =
    MAX_CHECKPOINT_HEADER_BYTES + MAX_CHECKPOINT_RECORD_BYTES * MAX_OBJECTS as u64;

fn checkpoint_should_persist(persisted: usize, completed: usize, yielding: bool) -> bool {
    completed > persisted
        && (yielding || completed.saturating_sub(persisted) >= CHECKPOINT_INTERVAL)
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedObjectError {
    /// The hard response-snapshot memory budget is already held by response
    /// bodies. Callers should reject admission promptly, not wait behind a
    /// client whose socket may remain stalled indefinitely.
    Capacity,
    Other(String),
}

impl VerifiedObjectError {
    pub fn is_capacity(&self) -> bool {
        matches!(self, Self::Capacity)
    }
}

impl std::fmt::Display for VerifiedObjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("authenticated response snapshot capacity full"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for VerifiedObjectError {}

impl From<String> for VerifiedObjectError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
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
    record_digest: String,
}

#[derive(Serialize)]
struct CheckpointRecordBody<'a> {
    object: &'a GenerationObject,
    fingerprint: &'a FileFingerprint,
}

fn canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn checkpoint_record_digest(
    object: &GenerationObject,
    fingerprint: &FileFingerprint,
) -> Result<String, String> {
    let encoded = serde_json::to_vec(&CheckpointRecordBody {
        object,
        fingerprint,
    })
    .map_err(|error| format!("serializing generation checkpoint authority: {error}"))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

impl CheckpointObject {
    fn new(object: GenerationObject, fingerprint: FileFingerprint) -> Result<Self, String> {
        let record_digest = checkpoint_record_digest(&object, &fingerprint)?;
        Ok(Self {
            object,
            fingerprint,
            record_digest,
        })
    }

    fn authority_is_valid(&self) -> bool {
        self.object.bytes == self.fingerprint.bytes
            && canonical_sha256(&self.object.sha256)
            && canonical_sha256(&self.record_digest)
            && checkpoint_record_digest(&self.object, &self.fingerprint)
                .is_ok_and(|actual| actual == self.record_digest)
    }
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

fn safe_generation_id(generation_id: &str) -> bool {
    !generation_id.is_empty()
        && generation_id.len() <= 256
        && generation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

async fn open_read_nofollow(path: &Path) -> std::io::Result<tokio::fs::File> {
    crate::fs_secure::open_read_nofollow(path).await
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

#[cfg(unix)]
fn secure_file_identity(metadata: &std::fs::Metadata) -> crate::fs_secure::FileIdentity {
    use std::os::unix::fs::MetadataExt;
    crate::fs_secure::FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
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

struct CheckpointValidationResume {
    key: String,
    checkpoint_identity: crate::fs_secure::FileIdentity,
    offset: u64,
    objects: Vec<CheckpointObject>,
}

const CHECKPOINT_RESUME_CACHE_BYTES: u64 = 96 * 1024 * 1024;
const CHECKPOINT_RESUME_CACHE_ENTRIES: usize = 4;

fn checkpoint_resume_cache() -> &'static std::sync::Mutex<VecDeque<CheckpointValidationResume>> {
    static CACHE: OnceLock<std::sync::Mutex<VecDeque<CheckpointValidationResume>>> =
        OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn take_checkpoint_resume(key: &str) -> Option<CheckpointValidationResume> {
    let mut cache = checkpoint_resume_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let position = cache.iter().position(|entry| entry.key == key)?;
    cache.remove(position)
}

fn remember_checkpoint_resume(entry: CheckpointValidationResume) {
    if entry.offset > CHECKPOINT_RESUME_CACHE_BYTES {
        return;
    }
    let mut cache = checkpoint_resume_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(position) = cache.iter().position(|current| current.key == entry.key) {
        cache.remove(position);
    }
    cache.push_back(entry);
    while cache.len() > CHECKPOINT_RESUME_CACHE_ENTRIES
        || cache
            .iter()
            .map(|entry| entry.offset)
            .fold(0_u64, u64::saturating_add)
            > CHECKPOINT_RESUME_CACHE_BYTES
    {
        cache.pop_front();
    }
}

/// Read one newline-delimited checkpoint record without allowing a malformed
/// checkpoint to turn the line buffer into a checkpoint-sized allocation.
async fn read_checkpoint_record<R>(
    reader: &mut R,
    max_bytes: u64,
) -> std::io::Result<Option<(Vec<u8>, bool)>>
where
    R: AsyncBufRead + Unpin,
{
    let mut record = Vec::with_capacity(max_bytes.min(4 * 1024) as usize);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if record.is_empty() {
                Ok(None)
            } else {
                Ok(Some((record, false)))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if record.len() as u64 + consumed as u64 > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checkpoint record exceeds its byte bound",
            ));
        }
        record.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some((record, true)));
        }
    }
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
    should_yield: F,
) -> Result<Option<GenerationManifest>, String>
where
    F: FnMut() -> bool,
{
    let directory = crate::fs_secure::SecureDirectory::open(root)
        .await
        .map_err(|error| format!("opening generation directory: {error}"))?;
    publish_controlled_directory(&directory, generation_id, ordered_names, should_yield).await
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
    let directory_identity = match root.identity().await {
        Ok(identity) => identity,
        Err(_) => {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
    };
    let key = format!(
        "{}:{}:{generation_id}",
        directory_identity.device, directory_identity.inode
    );
    let mut file = match root.open_read_child(CHECKPOINT_FILE).await {
        Ok(file) => file,
        Err(_) => {
            let _ = take_checkpoint_resume(&key);
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_CHECKPOINT_BYTES => metadata,
        _ => {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
    };
    let checkpoint_identity = secure_file_identity(&metadata);
    let mut resume = take_checkpoint_resume(&key)
        .filter(|resume| {
            resume.checkpoint_identity.same_inode(checkpoint_identity)
                && resume.offset <= checkpoint_identity.size
                && resume.objects.len() <= ordered_names.len()
                && resume
                    .objects
                    .iter()
                    .zip(ordered_names)
                    .all(|(completed, requested)| completed.object.name == *requested)
        })
        .unwrap_or_else(|| CheckpointValidationResume {
            key,
            checkpoint_identity,
            offset: 0,
            objects: Vec::new(),
        });
    // The append-only writer never rewrites an already-persisted prefix. An
    // inode replacement invalidates the cache above; growth on the same inode
    // preserves the validated cursor and lets a preempted worker make forward
    // progress instead of rescanning a large prefix on every retry.
    resume.checkpoint_identity = checkpoint_identity;
    if file
        .seek(std::io::SeekFrom::Start(resume.offset))
        .await
        .is_err()
    {
        return CheckpointLoad::Ready {
            objects: Vec::new(),
            needs_repair: true,
        };
    }
    let mut reader = tokio::io::BufReader::with_capacity(64 * 1024, file);
    if resume.offset == 0 {
        if should_yield() {
            remember_checkpoint_resume(resume);
            return CheckpointLoad::Yielded;
        }
        let Ok(Some((header_record, header_complete))) =
            read_checkpoint_record(&mut reader, MAX_CHECKPOINT_HEADER_BYTES).await
        else {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        };
        let header_line = header_record.strip_suffix(b"\n").unwrap_or(&header_record);
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
        resume.offset = header_record.len() as u64;
        if resume.offset > MAX_CHECKPOINT_BYTES {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
    }
    let mut needs_repair = false;
    loop {
        let record = match read_checkpoint_record(&mut reader, MAX_CHECKPOINT_RECORD_BYTES).await {
            Ok(Some(record)) => record,
            Ok(None) => break,
            Err(_) => {
                needs_repair = true;
                break;
            }
        };
        let (record, complete) = record;
        let Some(next_offset) = resume.offset.checked_add(record.len() as u64) else {
            needs_repair = true;
            break;
        };
        if next_offset > MAX_CHECKPOINT_BYTES {
            needs_repair = true;
            break;
        }
        let line = record.strip_suffix(b"\n").unwrap_or(&record);
        if line.is_empty() {
            needs_repair = true;
            break;
        }
        let Ok(completed) = serde_json::from_slice::<CheckpointObject>(line) else {
            needs_repair = true;
            break;
        };
        if !complete {
            needs_repair = true;
        }
        let index = resume.objects.len();
        if index >= ordered_names.len()
            || completed.object.name != ordered_names[index]
            || completed.object.bytes > MAX_OBJECT_BYTES
            || !completed.authority_is_valid()
        {
            return CheckpointLoad::Ready {
                objects: Vec::new(),
                needs_repair: true,
            };
        }
        if should_yield() {
            remember_checkpoint_resume(resume);
            return CheckpointLoad::Yielded;
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
        resume.objects.push(completed);
        resume.offset = next_offset;
        if !complete {
            break;
        }
    }
    match reader.get_ref().metadata().await {
        Ok(metadata) => {
            let final_identity = secure_file_identity(&metadata);
            if !metadata.is_file()
                || !final_identity.same_inode(checkpoint_identity)
                || final_identity.size != resume.offset
                || final_identity.size > MAX_CHECKPOINT_BYTES
            {
                needs_repair = true;
            } else {
                resume.checkpoint_identity = final_identity;
            }
        }
        Err(_) => needs_repair = true,
    }
    let objects = resume.objects;
    if !needs_repair {
        remember_checkpoint_resume(CheckpointValidationResume {
            key: resume.key,
            checkpoint_identity: resume.checkpoint_identity,
            offset: resume.offset,
            objects: objects.clone(),
        });
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
    _generation_id: &str,
    objects: &[CheckpointObject],
) -> Result<bool, String> {
    let mut encoded = Vec::new();
    for object in objects {
        serde_json::to_writer(&mut encoded, object)
            .map_err(|error| format!("serializing generation checkpoint: {error}"))?;
        encoded.push(b'\n');
    }
    root.append_bounded_child(CHECKPOINT_FILE, &encoded, MAX_CHECKPOINT_BYTES)
        .await
        .map_err(|error| format!("appending generation checkpoint: {error}"))
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
    while bytes < opened_fingerprint.bytes {
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
        if bytes < opened_fingerprint.bytes && should_yield() {
            return Ok(None);
        }
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

/// Recheck every checkpoint fingerprint immediately before publication.
///
/// A validation cursor may span cooperative worker invocations, so a file
/// validated before an earlier yield can no longer be treated as current
/// authority. This final descriptor-relative pass is deliberately not cached:
/// it is the publication boundary that makes reuse of the resumable parsed
/// prefix safe even if a staging object was replaced between retries.
async fn checkpoint_fingerprints_match_directory(
    root: &crate::fs_secure::SecureDirectory,
    completed: &[CheckpointObject],
) -> Result<bool, String> {
    for object in completed {
        let file = match root.open_read_child(&object.object.name).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "opening generation object {} for final validation: {error}",
                    object.object.name
                ));
            }
        };
        let metadata = file.metadata().await.map_err(|error| {
            format!(
                "reading generation object {} for final validation: {error}",
                object.object.name
            )
        })?;
        if !metadata.is_file() || fingerprint(&metadata) != object.fingerprint {
            return Ok(false);
        }
    }
    Ok(true)
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
    if !safe_generation_id(generation_id)
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
            if checkpoint_enabled && checkpoint_should_persist(persisted, completed.len(), true) {
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
            if checkpoint_enabled && checkpoint_should_persist(persisted, completed.len(), true) {
                if let Err(error) =
                    append_checkpoint_directory(root, generation_id, &completed[persisted..]).await
                {
                    tracing::warn!(%error, "generation digest checkpoint disabled");
                }
            }
            return Ok(None);
        };
        completed.push(CheckpointObject::new(object, fingerprint)?);
        if checkpoint_enabled && checkpoint_should_persist(persisted, completed.len(), false) {
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
        if checkpoint_enabled && checkpoint_should_persist(persisted, completed.len(), true) {
            let _ = append_checkpoint_directory(root, generation_id, &completed[persisted..]).await;
        }
        return Ok(None);
    }
    if !checkpoint_fingerprints_match_directory(root, &completed).await? {
        // The resumable parse cache is an optimization, never publication
        // authority. Replace the checkpoint inode so every cached cursor is
        // invalidated, then let the next cooperative invocation rehash the
        // generation from a clean header.
        replace_checkpoint_directory(root, generation_id, &[]).await?;
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
        || !safe_generation_id(&manifest.generation_id)
        || manifest.object_count != manifest.objects.len()
        || manifest.objects.is_empty()
        || manifest.objects.len() > MAX_OBJECTS
        || manifest.objects.iter().any(|object| {
            !safe_object_name(&object.name)
                || object.bytes > MAX_OBJECT_BYTES
                || (object.name == "index.m3u8" && object.bytes > MAX_MANIFEST_BYTES)
                || !canonical_sha256(&object.sha256)
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
        || !canonical_sha256(&manifest.manifest_digest)
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
    ) -> Result<Option<VerifiedObject>, VerifiedObjectError> {
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
        let permit = match budget.try_acquire_many_owned(units) {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                return Err(VerifiedObjectError::Capacity)
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                return Err(VerifiedObjectError::Other(
                    "authenticated response snapshot budget closed".to_owned(),
                ))
            }
        };
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
        // Tokio may report the final `write_all` as accepted while its
        // blocking file write is still in flight. Drain that write before
        // Linux seals the memfd; otherwise the seal can win the race, leave
        // an empty snapshot, and defer the write failure behind the seek.
        #[cfg(target_os = "linux")]
        snapshot
            .flush()
            .await
            .map_err(|error| format!("flushing authenticated snapshot: {error}"))?;
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

    async fn checkpoint_corruption_is_rehashed(mutate_bytes: bool) {
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
        assert!(
            publish_controlled_directory(&capability, "generation-corrupt", &names, || {
                calls += 1;
                calls >= 3
            })
            .await
            .expect("checkpointed publication")
            .is_none()
        );
        let encoded = capability
            .read_bounded_child(CHECKPOINT_FILE, MAX_CHECKPOINT_BYTES)
            .await
            .expect("checkpoint");
        let mut records = encoded
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| line.to_vec())
            .collect::<Vec<_>>();
        let mut record = serde_json::from_slice::<serde_json::Value>(&records[1])
            .expect("parse checkpoint record");
        if mutate_bytes {
            let bytes = record["object"]["bytes"].as_u64().expect("object bytes");
            record["object"]["bytes"] = serde_json::Value::from(bytes + 1);
        } else {
            let uppercase = record["object"]["sha256"]
                .as_str()
                .expect("object digest")
                .to_ascii_uppercase();
            record["object"]["sha256"] = serde_json::Value::from(uppercase);
        }
        records[1] = serde_json::to_vec(&record).expect("serialize corrupt record");
        let mut corrupt = Vec::new();
        for record in records {
            corrupt.extend_from_slice(&record);
            corrupt.push(b'\n');
        }
        capability
            .atomic_write_child(CHECKPOINT_FILE, &corrupt)
            .await
            .expect("publish corrupt checkpoint");

        let manifest =
            publish_controlled_directory(&capability, "generation-corrupt", &names, || false)
                .await
                .expect("resume publication")
                .expect("manifest");
        for name in &names {
            let bytes = tokio::fs::read(directory.path().join(name))
                .await
                .expect("object bytes");
            assert!(manifest.verify_bytes(name, &bytes));
        }
        assert!(manifest
            .objects
            .iter()
            .all(|object| canonical_sha256(&object.sha256)));
    }

    #[tokio::test]
    async fn parseable_checkpoint_digest_corruption_is_rehashed() {
        checkpoint_corruption_is_rehashed(false).await;
    }

    #[tokio::test]
    async fn parseable_checkpoint_byte_count_corruption_is_rehashed() {
        checkpoint_corruption_is_rehashed(true).await;
    }

    #[test]
    fn append_only_checkpoints_advance_under_repeated_small_yield_budgets() {
        const PER_PASS_BUDGET: usize = 1_000;
        let mut persisted = 0usize;
        let mut appended_objects = 0usize;
        let mut passes = 0usize;

        // Each retry starts from the durable prefix, just like the real
        // loader. A yield budget smaller than any previous geometric batch
        // must still make durable forward progress rather than livelock.
        while persisted < MAX_OBJECTS {
            passes += 1;
            let completed = persisted.saturating_add(PER_PASS_BUDGET).min(MAX_OBJECTS);
            if checkpoint_should_persist(persisted, completed, true) {
                appended_objects = appended_objects.saturating_add(completed - persisted);
                persisted = completed;
            }
            assert!(passes <= MAX_OBJECTS / PER_PASS_BUDGET + 1);
        }
        assert_eq!(persisted, MAX_OBJECTS);
        assert_eq!(appended_objects, MAX_OBJECTS);
    }

    fn maximum_sized_checkpoint_record() -> CheckpointObject {
        CheckpointObject::new(
            GenerationObject {
                name: "x".repeat(128),
                bytes: u64::MAX,
                sha256: "f".repeat(64),
            },
            FileFingerprint {
                bytes: u64::MAX,
                modified_secs: u64::MAX,
                modified_nanos: u32::MAX,
                #[cfg(unix)]
                device: u64::MAX,
                #[cfg(unix)]
                inode: u64::MAX,
                #[cfg(unix)]
                changed_secs: i64::MIN,
                #[cfg(unix)]
                changed_nanos: i64::MIN,
            },
        )
        .expect("bounded checkpoint record")
    }

    #[test]
    fn checkpoint_capacity_covers_every_permitted_object_record() {
        let record = maximum_sized_checkpoint_record();
        let record_bytes = serde_json::to_vec(&record).expect("serialize record").len() as u64 + 1;
        let header_bytes = serde_json::to_vec(&CheckpointHeader {
            format_version: FORMAT_VERSION,
            generation_id: "g".repeat(256),
        })
        .expect("serialize header")
        .len() as u64
            + 1;
        assert!(record_bytes <= MAX_CHECKPOINT_RECORD_BYTES);
        assert!(header_bytes <= MAX_CHECKPOINT_HEADER_BYTES);
        assert!(
            header_bytes.saturating_add(record_bytes.saturating_mul(MAX_OBJECTS as u64))
                <= MAX_CHECKPOINT_BYTES
        );
    }

    #[tokio::test]
    async fn checkpoint_appends_every_permitted_record_across_retries() {
        const BATCH_RECORDS: usize = 1_024;

        let directory = tempfile::tempdir().expect("generation directory");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        replace_checkpoint_directory(&capability, "generation-large", &[])
            .await
            .expect("seed checkpoint");
        let record = maximum_sized_checkpoint_record();
        let batch = vec![record; BATCH_RECORDS];
        let mut appended = 0_u64;
        while appended < MAX_OBJECTS as u64 {
            // Reopen the directory capability to model a new bounded worker
            // invocation resuming from the durable prefix.
            let retry = crate::fs_secure::SecureDirectory::open(directory.path())
                .await
                .expect("retry capability");
            let remaining = (MAX_OBJECTS as u64 - appended).min(BATCH_RECORDS as u64) as usize;
            assert!(
                append_checkpoint_directory(&retry, "generation-large", &batch[..remaining])
                    .await
                    .expect("append checkpoint batch")
            );
            appended += remaining as u64;
        }
        let checkpoint = capability
            .child_metadata(CHECKPOINT_FILE)
            .await
            .expect("checkpoint metadata");
        assert!(checkpoint.identity.size <= MAX_CHECKPOINT_BYTES);
    }

    #[test]
    fn maximum_permitted_inventory_fits_the_manifest_byte_contract() {
        let objects = (0..MAX_OBJECTS)
            .map(|index| GenerationObject {
                // Quotes are permitted safe-name characters and exercise the
                // maximum two-byte JSON escape expansion for all remaining
                // name bytes.
                name: format!("{index:08}-{}", "\"".repeat(119)),
                bytes: MAX_OBJECT_BYTES,
                sha256: "f".repeat(64),
            })
            .collect::<Vec<_>>();
        assert!(objects.iter().all(|object| safe_object_name(&object.name)));
        let generation_id = "g".repeat(256);
        let manifest = GenerationManifest {
            format_version: FORMAT_VERSION,
            generation_id: generation_id.clone(),
            object_count: objects.len(),
            manifest_digest: body_digest(&generation_id, &objects).expect("manifest digest"),
            objects,
        };
        let encoded = serde_json::to_vec(&manifest).expect("serialize maximum manifest");
        assert!(encoded.len() as u64 <= MAX_MANIFEST_BYTES);
    }

    #[tokio::test]
    async fn generation_id_rejects_json_expansion_outside_the_header_contract() {
        let directory = tempfile::tempdir().expect("generation directory");
        tokio::fs::write(directory.path().join("seg00000.ts"), b"segment")
            .await
            .expect("segment");
        let error = publish(
            directory.path(),
            &"\"".repeat(256),
            &["seg00000.ts".to_owned()],
        )
        .await
        .expect_err("unsafe generation id");
        assert!(error.contains("invalid generation manifest identity"));
    }

    #[tokio::test]
    async fn capability_checkpoint_validation_resumes_across_small_preemption_budgets() {
        const OBJECTS: usize = 128;
        const PROBES_PER_PASS: usize = 17;

        let directory = tempfile::tempdir().expect("generation directory");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let mut names = Vec::with_capacity(OBJECTS);
        let mut completed = Vec::with_capacity(OBJECTS);
        for index in 0..OBJECTS {
            let name = format!("seg{index:05}.ts");
            let bytes = format!("checkpoint object {index}").into_bytes();
            tokio::fs::write(directory.path().join(&name), &bytes)
                .await
                .expect("object bytes");
            let metadata = tokio::fs::metadata(directory.path().join(&name))
                .await
                .expect("object metadata");
            completed.push(
                CheckpointObject::new(
                    GenerationObject {
                        name: name.clone(),
                        bytes: bytes.len() as u64,
                        sha256: hex::encode(Sha256::digest(&bytes)),
                    },
                    fingerprint(&metadata),
                )
                .expect("checkpoint record"),
            );
            names.push(name);
        }
        replace_checkpoint_directory(&capability, "generation-resumable", &completed)
            .await
            .expect("seed checkpoint");

        for pass in 1..=20 {
            let mut probes = 0usize;
            match load_checkpoint_directory(
                &capability,
                "generation-resumable",
                &names,
                &mut || {
                    probes += 1;
                    probes > PROBES_PER_PASS
                },
            )
            .await
            {
                CheckpointLoad::Yielded => {}
                CheckpointLoad::Ready {
                    objects,
                    needs_repair,
                } => {
                    assert!(!needs_repair);
                    assert_eq!(objects, completed);
                    assert!(pass > 1, "the budget must force at least one preemption");
                    return;
                }
            }
        }
        panic!("checkpoint validation restarted instead of advancing its cached cursor");
    }

    #[tokio::test]
    async fn validated_checkpoint_cursor_survives_a_later_object_hash_yield() {
        const PREFIX: usize = 64;
        let directory = tempfile::tempdir().expect("generation directory");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let mut names = Vec::with_capacity(PREFIX + 1);
        let mut completed = Vec::with_capacity(PREFIX);
        for index in 0..PREFIX {
            let name = format!("seg{index:05}.ts");
            let bytes = format!("durable prefix {index}").into_bytes();
            tokio::fs::write(directory.path().join(&name), &bytes)
                .await
                .expect("prefix object");
            let metadata = tokio::fs::metadata(directory.path().join(&name))
                .await
                .expect("prefix metadata");
            completed.push(
                CheckpointObject::new(
                    GenerationObject {
                        name: name.clone(),
                        bytes: bytes.len() as u64,
                        sha256: hex::encode(Sha256::digest(&bytes)),
                    },
                    fingerprint(&metadata),
                )
                .expect("checkpoint object"),
            );
            names.push(name);
        }
        let final_name = format!("seg{PREFIX:05}.ts");
        tokio::fs::write(directory.path().join(&final_name), vec![0x71; 512 * 1024])
            .await
            .expect("unhashed suffix");
        names.push(final_name);
        replace_checkpoint_directory(&capability, "generation-post-load-yield", &completed)
            .await
            .expect("seed durable prefix");

        let mut probes = 0usize;
        let yielded =
            publish_controlled_directory(&capability, "generation-post-load-yield", &names, || {
                probes += 1;
                probes >= 67
            })
            .await
            .expect("yield after loading the durable prefix");
        assert!(yielded.is_none());

        let mut reload_probes = 0usize;
        let loaded = load_checkpoint_directory(
            &capability,
            "generation-post-load-yield",
            &names,
            &mut || {
                reload_probes += 1;
                false
            },
        )
        .await;
        let CheckpointLoad::Ready { objects, .. } = loaded else {
            panic!("cached EOF cursor unexpectedly yielded");
        };
        assert_eq!(objects.len(), PREFIX);
        assert!(
            reload_probes <= 2,
            "a post-load hash yield must not force {PREFIX} prefix records through validation again"
        );
    }

    #[tokio::test]
    async fn mutation_between_cached_validation_and_publication_forces_rehash() {
        let directory = tempfile::tempdir().expect("generation directory");
        let first_name = "seg00000.ts".to_owned();
        let second_name = "seg00001.ts".to_owned();
        tokio::fs::write(directory.path().join(&first_name), b"original prefix")
            .await
            .expect("prefix object");
        tokio::fs::write(directory.path().join(&second_name), vec![0x42; 512 * 1024])
            .await
            .expect("suffix object");
        let metadata = tokio::fs::metadata(directory.path().join(&first_name))
            .await
            .expect("prefix metadata");
        let completed = CheckpointObject::new(
            GenerationObject {
                name: first_name.clone(),
                bytes: b"original prefix".len() as u64,
                sha256: hex::encode(Sha256::digest(b"original prefix")),
            },
            fingerprint(&metadata),
        )
        .expect("checkpoint object");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        replace_checkpoint_directory(
            &capability,
            "generation-mutated",
            std::slice::from_ref(&completed),
        )
        .await
        .expect("seed durable prefix");
        let names = vec![first_name.clone(), second_name];
        let mut probes = 0usize;
        assert!(
            publish_controlled_directory(&capability, "generation-mutated", &names, || {
                probes += 1;
                probes >= 5
            })
            .await
            .expect("yield after prefix validation")
            .is_none()
        );

        tokio::fs::write(directory.path().join(&first_name), b"replacement prefix")
            .await
            .expect("mutate cached prefix");
        assert!(
            publish_controlled_directory(&capability, "generation-mutated", &names, || false)
                .await
                .expect("final fingerprint validation")
                .is_none(),
            "a stale cached digest must clear its checkpoint instead of publishing"
        );
        let manifest =
            publish_controlled_directory(&capability, "generation-mutated", &names, || false)
                .await
                .expect("rehash replacement")
                .expect("publish repaired generation");
        assert!(manifest.verify_bytes(&first_name, b"replacement prefix"));
        assert!(!manifest.verify_bytes(&first_name, b"original prefix"));
    }

    #[tokio::test]
    async fn cached_checkpoint_cursor_is_bound_to_the_requested_object_prefix() {
        let directory = tempfile::tempdir().expect("generation directory");
        for name in ["seg00000.ts", "seg00001.ts", "seg00002.ts"] {
            tokio::fs::write(directory.path().join(name), name.as_bytes())
                .await
                .expect("object bytes");
        }
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let mut completed = Vec::new();
        for name in ["seg00000.ts", "seg00001.ts"] {
            let bytes = name.as_bytes();
            let metadata = tokio::fs::metadata(directory.path().join(name))
                .await
                .expect("object metadata");
            completed.push(
                CheckpointObject::new(
                    GenerationObject {
                        name: name.to_owned(),
                        bytes: bytes.len() as u64,
                        sha256: hex::encode(Sha256::digest(bytes)),
                    },
                    fingerprint(&metadata),
                )
                .expect("checkpoint object"),
            );
        }
        replace_checkpoint_directory(&capability, "generation-list", &completed)
            .await
            .expect("seed checkpoint");
        let original = vec!["seg00000.ts".to_owned(), "seg00001.ts".to_owned()];
        assert!(matches!(
            load_checkpoint_directory(&capability, "generation-list", &original, &mut || false)
                .await,
            CheckpointLoad::Ready { objects, .. } if objects.len() == 2
        ));

        let changed = vec!["seg00002.ts".to_owned(), "seg00001.ts".to_owned()];
        assert!(matches!(
            load_checkpoint_directory(&capability, "generation-list", &changed, &mut || false)
                .await,
            CheckpointLoad::Ready { objects, needs_repair: true } if objects.is_empty()
        ));
        let shortened = vec!["seg00000.ts".to_owned()];
        assert!(matches!(
            load_checkpoint_directory(&capability, "generation-list", &shortened, &mut || false)
                .await,
            CheckpointLoad::Ready { objects, needs_repair: true } if objects.is_empty()
        ));
    }

    #[tokio::test]
    async fn checkpoint_growth_after_open_is_rejected_without_a_large_line_allocation() {
        let directory = tempfile::tempdir().expect("generation directory");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        replace_checkpoint_directory(&capability, "generation-growing", &[])
            .await
            .expect("seed checkpoint");
        let checkpoint_path = directory.path().join(CHECKPOINT_FILE);
        let mut grew = false;
        let loaded = load_checkpoint_directory(
            &capability,
            "generation-growing",
            &["seg00000.ts".to_owned()],
            &mut || {
                if !grew {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&checkpoint_path)
                        .expect("open checkpoint writer")
                        .set_len(MAX_CHECKPOINT_BYTES + 1)
                        .expect("grow checkpoint after validation stat");
                    grew = true;
                }
                false
            },
        )
        .await;
        assert!(grew);
        assert!(matches!(
            loaded,
            CheckpointLoad::Ready {
                needs_repair: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn blank_checkpoint_tail_is_rejected_in_constant_record_work() {
        let directory = tempfile::tempdir().expect("generation directory");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let header = CheckpointHeader {
            format_version: FORMAT_VERSION,
            generation_id: "generation-blank".to_owned(),
        };
        let mut encoded = serde_json::to_vec(&header).expect("header");
        encoded.push(b'\n');
        encoded.extend(std::iter::repeat_n(b'\n', 1024 * 1024));
        capability
            .atomic_write_child(CHECKPOINT_FILE, &encoded)
            .await
            .expect("blank-tail checkpoint");
        let mut probes = 0usize;
        let loaded = load_checkpoint_directory(
            &capability,
            "generation-blank",
            &["seg00000.ts".to_owned()],
            &mut || {
                probes += 1;
                false
            },
        )
        .await;
        assert!(matches!(
            loaded,
            CheckpointLoad::Ready {
                needs_repair: true,
                ..
            }
        ));
        assert!(probes <= 2, "blank records must not consume one probe each");
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
            calls >= 3
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

    #[tokio::test]
    async fn zero_length_object_yields_before_it_is_checkpointed() {
        let directory = tempfile::tempdir().expect("generation directory");
        let name = "seg00000.ts".to_owned();
        tokio::fs::write(directory.path().join(&name), b"")
            .await
            .expect("empty segment");
        let capability = crate::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("directory capability");
        let mut probes = 0usize;
        let yielded =
            publish_controlled_directory(&capability, "generation-empty", &[name], || {
                probes += 1;
                true
            })
            .await
            .expect("yield empty generation");
        assert!(yielded.is_none());
        assert_eq!(probes, 1);
        assert!(!directory.path().join(MANIFEST_FILE).exists());
        let checkpoint = capability
            .read_bounded_child(CHECKPOINT_FILE, MAX_CHECKPOINT_BYTES)
            .await
            .expect("header-only checkpoint");
        assert_eq!(
            checkpoint
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            1,
            "an empty object must not become durable progress after yielding"
        );
    }

    #[tokio::test]
    async fn stalled_snapshot_bodies_reject_same_class_admission_promptly() {
        let directory = tempfile::tempdir().expect("generation directory");
        let bytes = vec![0x5a; (RESPONSE_SMALL_MAX_MIB as usize + 1) * 1024 * 1024];
        tokio::fs::write(directory.path().join("seg00000.ts"), &bytes)
            .await
            .expect("medium segment");
        let manifest = GenerationManifest {
            format_version: FORMAT_VERSION,
            generation_id: "capacity-test".to_owned(),
            object_count: 1,
            objects: vec![GenerationObject {
                name: "seg00000.ts".to_owned(),
                bytes: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(&bytes)),
            }],
            manifest_digest: String::new(),
        };
        let budget = RESPONSE_MEDIUM_BUDGET
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_MEDIUM_BUDGET_MIB)))
            .clone();
        let stalled_body = budget
            .acquire_many_owned(RESPONSE_MEDIUM_BUDGET_MIB as u32)
            .await
            .expect("hold medium response class");

        let observed = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            manifest.open_verified_object(directory.path(), "seg00000.ts"),
        )
        .await
        .expect("admission result must be prompt");
        assert!(matches!(observed, Err(VerifiedObjectError::Capacity)));
        drop(stalled_body);
    }
}
