//! Process-local read proof, deliberately separate from inference readiness.
//!
//! A persisted inode/boot receipt cannot prove bytes after a process/OS restart.
//! Authorized readers may trigger this full held-file hash, without creating a
//! job, demand, model receipt, or principal. One global I/O lane and one normal
//! speculative CPU slot bound work. No timer or daemon resumes it: when live
//! playback needs capacity, the next authorized read resumes the held prefix.
use super::runtime::SourceHash;
use crate::{
    fragment_index_cluster::{open_source_playback_fence, SourceFence},
    http::error::ApiError,
    state::AppState,
};
use axum::http::StatusCode;
use plurx_core::{
    domain::MediaFile,
    fs_secure::{std_file_identity, FileIdentity},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_ENTRIES: usize = 64;
static HASH_IO: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Clone, PartialEq, Eq)]
struct Key {
    file_id: i64,
    size: i64,
    mtime: i64,
    path: PathBuf,
    sha256: String,
    identity: FileIdentity,
    object_version: String,
}
struct Work {
    fence: SourceFence,
    hash: SourceHash,
}
enum Status {
    Running,
    Paused(Work),
    Ready(SourceFence),
    Failed { changed: bool, retry_at: Instant },
}
struct Entry {
    key: Key,
    status: Status,
}
pub(crate) struct ReadAttestations {
    entries: Mutex<VecDeque<Entry>>,
    cancel: CancellationToken,
}
impl Drop for ReadAttestations {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
fn pending() -> ApiError {
    ApiError::TypedRetry {
        status: StatusCode::TOO_EARLY,
        code: "source_verifying",
        message: "Source bytes are being verified; retry this read".into(),
        retry_after_seconds: 1,
    }
}
fn changed() -> ApiError {
    ApiError::typed(StatusCode::GONE, "source_changed", "The source changed")
}
impl ReadAttestations {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(VecDeque::new()),
            cancel: CancellationToken::new(),
        })
    }
    fn entries(&self) -> std::sync::MutexGuard<'_, VecDeque<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn lookup(&self, key: &Key) -> Option<Result<(), ApiError>> {
        let mut entries = self.entries();
        if let Some(index) = entries.iter().position(|entry| entry.key == *key) {
            // Every cache hit has already reopened the current pathname and
            // compared its complete held identity and object version.
            match &entries[index].status {
                Status::Ready(fence)
                    if fence.unchanged()
                        && std_file_identity(&fence.handle).ok() == Some(key.identity) =>
                {
                    if let Some(entry) = entries.remove(index) {
                        entries.push_back(entry);
                    }
                    return Some(Ok(()));
                }
                Status::Ready(_) => {
                    entries.remove(index);
                }
                Status::Running => return Some(Err(pending())),
                Status::Failed { changed: true, .. } => return Some(Err(changed())),
                Status::Failed { retry_at, .. } if *retry_at > Instant::now() => {
                    return Some(Err(pending()))
                }
                Status::Failed { .. } | Status::Paused(_) => {}
            }
        }
        None
    }
    fn start(&self, key: &Key, fresh: Work) -> Result<Work, ApiError> {
        let mut entries = self.entries();
        if let Some(index) = entries.iter().position(|entry| entry.key == *key) {
            match &entries[index].status {
                Status::Running | Status::Ready(_) => return Err(pending()),
                Status::Failed { changed: true, .. } => return Err(changed()),
                Status::Failed { retry_at, .. } if *retry_at > Instant::now() => {
                    return Err(pending())
                }
                _ => {}
            }
            return Ok(
                match std::mem::replace(&mut entries[index].status, Status::Running) {
                    Status::Paused(work) => work,
                    _ => fresh,
                },
            );
        }
        if entries.len() >= MAX_ENTRIES {
            let idle = entries
                .iter()
                .position(|entry| !matches!(entry.status, Status::Running))
                .ok_or_else(pending)?;
            entries.remove(idle);
        }
        entries.push_back(Entry {
            key: key.clone(),
            status: Status::Running,
        });
        Ok(fresh)
    }
    /// This method grants no read authority. Both local and grant-bound callers
    /// must apply their own authorization before and after using this proof.
    pub(crate) async fn authorize(
        self: &Arc<Self>,
        state: &AppState,
        file: &MediaFile,
        sha256: &str,
    ) -> Result<(), ApiError> {
        if state.shutdown.is_cancelled() || self.cancel.is_cancelled() {
            return Err(pending());
        }
        let opened = tokio::time::timeout(
            Duration::from_secs(2),
            open_source_playback_fence(file, None),
        )
        .await
        .map_err(|_| pending())?
        .map_err(|_| changed())?;
        let identity = std_file_identity(&opened.handle).map_err(|_| changed())?;
        if u64::try_from(file.size).ok() != Some(identity.size) || !opened.unchanged() {
            return Err(changed());
        }
        let key = Key {
            file_id: file.id,
            size: file.size,
            mtime: file.mtime,
            path: file.path.clone(),
            sha256: sha256.into(),
            identity,
            object_version: opened.object_version().into(),
        };
        if let Some(result) = self.lookup(&key) {
            return result;
        }
        let limits = state.transcode.read_attestation_limits().await;
        let lane = Arc::clone(&HASH_IO)
            .try_acquire_owned()
            .map_err(|_| pending())?;
        let permit = state
            .transcode
            .admit_read_attestation(limits)
            .ok_or_else(pending)?;
        let fresh = Work {
            hash: SourceHash::new(opened.handle.try_clone().map_err(|_| changed())?, identity)
                .map_err(|_| changed())?,
            fence: opened,
        };
        let work = self.start(&key, fresh)?;
        let owner = Arc::downgrade(self);
        let cancel = self.cancel.child_token();
        let shutdown = state.shutdown.clone();
        let store = Arc::clone(&state.store);
        let transcode = Arc::clone(&state.transcode);
        // The owned task survives request cancellation. It holds no AppState or
        // strong cache owner; dropping Runtime cancels it without an Arc cycle.
        tokio::spawn(async move {
            let mut work = work;
            let mut lane = lane;
            let mut permit = permit;
            loop {
                let chunk_cancel = cancel.clone();
                let mut chunk = tokio::task::spawn_blocking(move || {
                    // Both guards live in this closure until the actual read
                    // ends, even when shutdown cancels the outer future.
                    let _cpu = permit;
                    let result = work.hash.step(&chunk_cancel);
                    (work, result, lane)
                });
                let joined = tokio::select! {
                    result = &mut chunk => result,
                    () = shutdown.cancelled() => { cancel.cancel(); chunk.await },
                    () = cancel.cancelled() => chunk.await,
                };
                let (next, result, io) = match joined {
                    Ok(value) => value,
                    Err(_) => {
                        finish(&owner, &key, failed(false));
                        return;
                    }
                };
                work = next;
                lane = io;
                if cancel.is_cancelled() || shutdown.is_cancelled() {
                    finish(&owner, &key, failed(false));
                    return;
                }
                match result {
                    Err(error) => {
                        finish(
                            &owner,
                            &key,
                            failed(error.to_string().contains("source_changed")),
                        );
                        return;
                    }
                    Ok(Some(digest)) => {
                        // Re-read the canonical record and named object after
                        // hashing, before retaining any process-local proof.
                        let valid = digest == key.sha256
                            && work.fence.unchanged()
                            && final_source(&*store, &key).await;
                        finish(
                            &owner,
                            &key,
                            if valid {
                                Status::Ready(work.fence)
                            } else {
                                failed(true)
                            },
                        );
                        return;
                    }
                    Ok(None) => {}
                }
                tokio::task::yield_now().await;
                let Some(next) = transcode.admit_read_attestation(limits) else {
                    finish(&owner, &key, Status::Paused(work));
                    return;
                };
                permit = next;
            }
        });
        Err(pending())
    }
}
fn failed(changed: bool) -> Status {
    Status::Failed {
        changed,
        retry_at: Instant::now() + Duration::from_secs(5),
    }
}
fn finish(owner: &Weak<ReadAttestations>, key: &Key, status: Status) {
    if let Some(owner) = owner.upgrade() {
        let mut entries = owner.entries();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.key == *key) {
            entry.status = status;
        }
    }
}
async fn final_source(store: &dyn plurx_core::store::Store, key: &Key) -> bool {
    let Ok(Ok(Some(file))) =
        tokio::time::timeout(Duration::from_secs(2), store.get_file(key.file_id)).await
    else {
        return false;
    };
    if file.size != key.size || file.mtime != key.mtime || file.path != key.path {
        return false;
    }
    let Ok(Ok(fence)) = tokio::time::timeout(
        Duration::from_secs(2),
        open_source_playback_fence(&file, Some(&key.object_version)),
    )
    .await
    else {
        return false;
    };
    fence.unchanged() && std_file_identity(&fence.handle).ok() == Some(key.identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    fn file(path: PathBuf) -> MediaFile {
        let metadata = std::fs::metadata(&path).expect("source metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        MediaFile {
            id: 41,
            item_id: 7,
            path,
            size: metadata.len() as i64,
            mtime,
            duration_ms: Some(1000),
            container: Some("mkv".into()),
            video_codec: None,
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: None,
            height: None,
            bit_depth: None,
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            dolby_vision: Default::default(),
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            downloaded_subtitles: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        }
    }
    async fn work(file: &MediaFile, expected: &str) -> (Key, Work) {
        let fence = open_source_playback_fence(file, None)
            .await
            .expect("held source");
        let identity = std_file_identity(&fence.handle).expect("identity");
        let key = Key {
            file_id: file.id,
            size: file.size,
            mtime: file.mtime,
            path: file.path.clone(),
            sha256: expected.into(),
            identity,
            object_version: fence.object_version().into(),
        };
        let hash = SourceHash::new(fence.handle.try_clone().expect("hash handle"), identity)
            .expect("hash state");
        (key, Work { fence, hash })
    }
    #[tokio::test]
    async fn restarted_reader_requires_full_hash_and_held_cache_rejects_replacement() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("source.mkv");
        let bytes = vec![37_u8; 128 * 1024 + 17];
        std::fs::write(&path, &bytes).expect("source");
        let expected = hex::encode(Sha256::digest(&bytes));
        // No process boot ID, old producer receipt, admin, demand or model is
        // available: only the immutable asset's expected full byte digest.
        let cache = ReadAttestations::new();
        let media = file(path.clone());
        let (key, fresh) = work(&media, &expected).await;
        assert!(cache.lookup(&key).is_none());
        let mut hashing = cache.start(&key, fresh).expect("single flight");
        assert!(hashing
            .hash
            .step(&cache.cancel)
            .expect("first chunk")
            .is_none());
        finish(&Arc::downgrade(&cache), &key, Status::Paused(hashing));
        let (_, fresh) = work(&media, &expected).await;
        let mut resumed = cache.start(&key, fresh).expect("resume held prefix");
        assert!(resumed
            .hash
            .step(&cache.cancel)
            .expect("last data")
            .is_none());
        let digest = resumed
            .hash
            .step(&cache.cancel)
            .expect("EOF")
            .expect("digest");
        assert_eq!(
            digest, expected,
            "resume must not restart or skip the prefix"
        );
        finish(&Arc::downgrade(&cache), &key, Status::Ready(resumed.fence));
        assert!(cache.lookup(&key).expect("retained proof").is_ok());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, &bytes).expect("same-sized replacement");
        std::fs::rename(replacement, &path).expect("replace named object");
        let (replacement_key, _) = work(&file(path), &expected).await;
        assert!(
            cache.lookup(&replacement_key).is_none(),
            "same bytes/scan revision do not reuse another physical identity"
        );
        let restarted = ReadAttestations::new();
        assert!(
            restarted.lookup(&replacement_key).is_none(),
            "an in-memory proof cannot survive restart as an inode receipt"
        );
    }
    #[tokio::test]
    async fn concurrent_reads_single_flight_and_cache_budget_preserve_live_work() {
        let mut source = tempfile::NamedTempFile::new().expect("source");
        source.write_all(b"one physical source").expect("bytes");
        let media = file(source.path().into());
        let cache = ReadAttestations::new();
        let (key, fresh) = work(&media, &"a".repeat(64)).await;
        let _owned = cache.start(&key, fresh).expect("first reader");
        let (_, duplicate) = work(&media, &key.sha256).await;
        assert_eq!(
            cache
                .start(&key, duplicate)
                .err()
                .expect("no duplicate")
                .code(),
            Some("source_verifying")
        );
        for index in 1..MAX_ENTRIES {
            let mut other = key.clone();
            other.file_id += index as i64;
            cache.entries().push_back(Entry {
                key: other,
                status: Status::Running,
            });
        }
        let mut extra = key.clone();
        extra.file_id = 9000;
        let (_, fresh) = work(&media, &key.sha256).await;
        assert!(cache.start(&extra, fresh).is_err());
        assert_eq!(cache.entries().len(), MAX_ENTRIES);
        assert!(matches!(cache.entries()[0].status, Status::Running));
        cache.entries()[1].status = failed(false);
        let (_, fresh) = work(&media, &key.sha256).await;
        assert!(
            cache.start(&extra, fresh).is_ok(),
            "only inactive entries can evict"
        );
        assert_eq!(cache.entries().len(), MAX_ENTRIES);
        finish(&Arc::downgrade(&cache), &key, failed(true));
        assert_eq!(
            cache
                .lookup(&key)
                .expect("failure")
                .expect_err("closed")
                .code(),
            Some("source_changed")
        );
    }
    #[test]
    fn dropping_runtime_cancels_owned_read_work_without_strong_owner_cycle() {
        let cache = ReadAttestations::new();
        let weak = Arc::downgrade(&cache);
        let task_cancel = cache.cancel.child_token();
        drop(cache);
        assert!(weak.upgrade().is_none());
        assert!(task_cancel.is_cancelled());
        assert_eq!(pending().code(), Some("source_verifying"));
    }
}
