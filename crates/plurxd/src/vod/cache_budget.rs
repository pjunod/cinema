//! Local ownership checks for shared playback-cache pressure eviction.
use super::*;
use crate::playback_cache::{CacheCandidate, CacheOwner, PlaybackCacheBudget};

struct VodCacheOwner(Weak<Shared>);

impl VodServe {
    pub(crate) fn register_playback_cache(self: &Arc<Self>, budget: &Arc<PlaybackCacheBudget>) {
        budget.register(Arc::new(VodCacheOwner(Arc::downgrade(&self.shared))));
    }
}

fn managed_key(key: &str) -> bool {
    let hash = key.strip_prefix("source-").unwrap_or(key);
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn quarantine_key(name: &str) -> Option<&str> {
    let (key, nonce) = name.strip_prefix(".plurx-vod-delete-")?.rsplit_once('-')?;
    (managed_key(key) && nonce.len() == 32 && uuid::Uuid::parse_str(nonce).is_ok()).then_some(key)
}

fn owned_member(name: &str) -> bool {
    name == INIT_NAME
        || name == IDENTITY_NAME
        || name == ENCODED_PROCESS_NAME
        || name == "identity.json.tmp"
        || name == "encoded-process.tmp"
        || name == "init.mp4.tmp"
        || planned_index(name).is_some()
        || name
            .strip_suffix(".tmp")
            .is_some_and(|name| planned_index(name).is_some())
}

const MAX_RENDITION_MEMBERS: usize = 65_536;

struct OrdinaryDirectory {
    path: PathBuf,
    name: String,
    parent: plurx_core::fs_secure::SecureDirectory,
    parent_identity: plurx_core::fs_secure::FileIdentity,
    directory: plurx_core::fs_secure::SecureDirectory,
    identity: plurx_core::fs_secure::FileIdentity,
}

async fn owned_members(
    directory: &plurx_core::fs_secure::SecureDirectory,
) -> io::Result<Option<Vec<(String, plurx_core::fs_secure::FileIdentity)>>> {
    let mut members = Vec::new();
    for name in directory.child_names(MAX_RENDITION_MEMBERS).await? {
        let metadata = directory.child_metadata(&name).await?;
        if !owned_member(&name) || !metadata.is_file {
            return Ok(None);
        }
        members.push((name, metadata.identity));
    }
    Ok(Some(members))
}

/// Hold both parent and child before Store awaits. Paths are never deletion
/// authority, even when a different process renames either directory.
async fn bind_ordinary_directory(path: &Path) -> io::Result<Option<OrdinaryDirectory>> {
    let Some(parent_path) = path.parent() else {
        return Ok(None);
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let parent = match plurx_core::fs_secure::SecureDirectory::open(parent_path).await {
        Ok(parent) => parent,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let parent_identity = parent.identity().await?;
    let directory = match parent.open_child_directory(name).await {
        Ok(directory) => directory,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let identity = directory.identity().await?;
    if owned_members(&directory).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(OrdinaryDirectory {
        path: path.to_owned(),
        name: name.to_owned(),
        parent,
        parent_identity,
        directory,
        identity,
    }))
}

#[cfg(test)]
async fn ordinary_directory(path: &Path) -> io::Result<bool> {
    Ok(bind_ordinary_directory(path).await?.is_some())
}

async fn remove_ordinary_directory(bound: &OrdinaryDirectory) -> io::Result<bool> {
    // A changed configured parent is not a reason to chase its replacement.
    let current = match plurx_core::fs_secure::SecureDirectory::open(
        bound.path.parent().expect("bound parent"),
    )
    .await
    {
        Ok(parent) => parent,
        Err(_) => return Ok(false),
    };
    if !current.identity().await?.same_inode(bound.parent_identity) {
        return Ok(false);
    }
    let current = bound.parent.child_metadata(&bound.name).await?;
    if !current.is_directory || !current.identity.same_inode(bound.identity) {
        return Ok(false);
    }
    let key = quarantine_key(&bound.name).unwrap_or(&bound.name);
    let quarantine = format!(".plurx-vod-delete-{key}-{}", uuid::Uuid::new_v4().simple());
    if !bound
        .parent
        .rename_child_noreplace(&bound.name, &quarantine)
        .await?
    {
        return Ok(false);
    }
    let remove = async {
        let current = bound.parent.child_metadata(&quarantine).await?;
        if !current.is_directory || !current.identity.same_inode(bound.identity) {
            return Ok(false);
        }
        // The held child is the inspected inode, not a reopened quarantine
        // pathname. Validate every member again before the first unlink.
        let Some(members) = owned_members(&bound.directory).await? else {
            return Ok(false);
        };
        for (name, identity) in members {
            let metadata = bound.directory.child_metadata(&name).await?;
            if !metadata.is_file || !metadata.identity.same_inode(identity) {
                return Ok(false);
            }
            bound.directory.unlink_child(&name).await?;
        }
        bound
            .parent
            .remove_empty_child_directory(&quarantine, bound.identity)
            .await?;
        Ok(true)
    }
    .await;
    if !matches!(remove, Ok(true)) {
        // Never overwrite a replacement published under the original name.
        let _ = bound
            .parent
            .rename_child_noreplace(&quarantine, &bound.name)
            .await;
    }
    remove
}

#[async_trait::async_trait]
impl CacheOwner for VodCacheOwner {
    async fn candidates(&self) -> io::Result<Vec<CacheCandidate>> {
        let Some(shared) = self.0.upgrade() else {
            return Ok(Vec::new());
        };
        let mut result = shared
            .retained_artifacts
            .pressure_candidates(&shared.base)
            .await;
        let mut entries = match tokio::fs::read_dir(&shared.base).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
            Err(error) => return Err(error),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !managed_key(&name) && quarantine_key(&name).is_none() {
                continue;
            }
            let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                let live = shared
                    .renditions
                    .lock()
                    .await
                    .get(&entry.file_name().to_string_lossy().into_owned())
                    .cloned();
                let last_used = live
                    .as_ref()
                    .and_then(|rendition| {
                        rendition
                            .dormant_since
                            .lock()
                            .expect("dormant lock")
                            .map(|since| {
                                SystemTime::now()
                                    .checked_sub(since.elapsed())
                                    .unwrap_or(UNIX_EPOCH)
                            })
                    })
                    .unwrap_or_else(|| metadata.modified().unwrap_or(UNIX_EPOCH));
                result.push(CacheCandidate {
                    path: entry.path(),
                    last_used,
                });
            }
        }
        Ok(result)
    }

    async fn evict(&self, candidate: &CacheCandidate) -> io::Result<bool> {
        let Some(shared) = self.0.upgrade() else {
            return Ok(false);
        };
        if candidate.path.parent() == Some(shared.base.join(".retained").as_path()) {
            return shared
                .retained_artifacts
                .pressure_evict(&candidate.path)
                .await;
        }
        if candidate.path.parent() != Some(shared.base.as_path()) {
            return Ok(false);
        }
        let Some(key) = candidate.path.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        let recovered = quarantine_key(key);
        let key = recovered.unwrap_or(key);
        if !managed_key(key) {
            return Ok(false);
        }
        // Writers and attachments own this same gate. Never queue behind a
        // publisher while a write reservation is asking us to release space.
        let Ok(_gate) = shared.rendition_build_gate(key).try_lock_owned() else {
            return Ok(false);
        };
        let Some(bound) = bind_ordinary_directory(&candidate.path).await? else {
            return Ok(false);
        };
        match tokio::time::timeout(
            Duration::from_secs(1),
            shared.store.quality_reserved_intervals(key),
        )
        .await
        {
            Ok(Ok(intervals)) if intervals.is_empty() => {}
            _ => return Ok(false),
        }
        let rendition = if recovered.is_some() {
            None
        } else {
            shared.renditions.lock().await.get(key).cloned()
        };
        let mut charged = None;
        if let Some(rendition) = rendition.as_ref() {
            if rendition.private_storage.is_some()
                || rendition.preparation().is_some()
                || !rendition.readers.lock().await.is_empty()
                || !shared.pool.retained(key).is_empty()
                || !matches!(rendition.slot.try_belief(), Some(Producer::Absent { .. }))
                // Registry, idle driver and this local clone are the only
                // expected references. Assembly or a GET keeps another one.
                || Arc::strong_count(rendition) > 3
            {
                return Ok(false);
            }
            let manifest = rendition.manifest.lock().await;
            charged = Some((manifest.materialized_bytes(), manifest.is_admitted()));
            rendition.closed.store(true, Release);
            rendition.gen_epoch.fetch_add(1, Relaxed);
        }
        // Removing names first preserves the charge if unlink fails. A
        // failed closed entry remains discoverable for the next pressure pass.
        if !remove_ordinary_directory(&bound).await? {
            return Ok(false);
        }
        if let Some(rendition) = rendition {
            let mut registry = shared.renditions.lock().await;
            if registry
                .get(key)
                .is_some_and(|entry| Arc::ptr_eq(entry, &rendition))
            {
                registry.remove(key);
                if let Some((bytes, admitted)) = charged {
                    sub_saturating(
                        if admitted {
                            &shared.completed_cache
                        } else {
                            &shared.working_set
                        },
                        bytes,
                    );
                }
            }
            drop(registry);
            shared.pool.close(key);
            rendition.kick();
        }
        shared.kick_all();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pressure_directory_requires_owned_regular_members() {
        let temp = crate::test_tempdir().expect("canonical temporary root");
        tokio::fs::write(temp.path().join("seg00000.m4s"), b"media")
            .await
            .expect("media");
        assert!(ordinary_directory(temp.path()).await.expect("inventory"));
        tokio::fs::write(
            temp.path().join(super::super::preparation_storage::MARKER),
            b"private",
        )
        .await
        .expect("marker");
        assert!(!ordinary_directory(temp.path())
            .await
            .expect("private inventory"));
        tokio::fs::remove_file(temp.path().join(super::super::preparation_storage::MARKER))
            .await
            .expect("remove marker");
        tokio::fs::create_dir(temp.path().join("unexpected"))
            .await
            .expect("nested");
        assert!(!ordinary_directory(temp.path())
            .await
            .expect("nested inventory"));
    }

    #[test]
    fn pressure_namespace_includes_source_and_rejects_unrelated_data() {
        assert!(managed_key(&"a".repeat(64)));
        assert!(managed_key(&format!("source-{}", "b".repeat(64))));
        assert!(!managed_key(".retained"));
        assert!(!managed_key("credentials"));
        assert!(!owned_member(".preparation-owner.json"));
    }
}

#[cfg(test)]
mod pressure_regressions {
    use super::*;

    #[tokio::test]
    async fn pressure_recovers_quarantine_after_restart_and_preserves_private_markers() {
        let temp = crate::test_tempdir().expect("cache");
        let key = "e".repeat(64);
        let path = temp.path().join(&key);
        tokio::fs::create_dir(&path).await.expect("directory");
        tokio::fs::write(path.join("seg00000.m4s"), b"owned")
            .await
            .expect("media");
        let quarantine = temp.path().join(format!(
            ".plurx-vod-delete-{key}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::rename(&path, &quarantine)
            .await
            .expect("crash after rename");
        let private = temp.path().join(format!(
            ".plurx-vod-delete-{}-{}",
            "f".repeat(64),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir(&private)
            .await
            .expect("private directory");
        tokio::fs::write(
            private.join(super::super::preparation_storage::MARKER),
            b"owned private",
        )
        .await
        .expect("marker");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(temp.path().to_path_buf(), store);
        let owner = VodCacheOwner(Arc::downgrade(&serve.shared));
        let candidates = owner.candidates().await.expect("restart inventory");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.path == quarantine)
            .expect("recoverable quarantine");
        assert!(owner.evict(candidate).await.expect("quarantine cleanup"));
        assert!(!quarantine.exists());
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.path == private)
            .expect("private quarantine");
        assert!(!owner.evict(candidate).await.expect("private preserved"));
        assert!(private
            .join(super::super::preparation_storage::MARKER)
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pressure_refuses_parent_or_child_substitution_after_validation() {
        let temp = crate::test_tempdir().expect("cache");
        let base = temp.path().join("base");
        let outside = temp.path().join("outside");
        let key = "d".repeat(64);
        for path in [base.join(&key), outside.join(&key)] {
            tokio::fs::create_dir_all(path).await.expect("tree");
        }
        tokio::fs::write(base.join(&key).join("seg00000.m4s"), b"owned")
            .await
            .expect("media");
        tokio::fs::write(outside.join(&key).join("keep"), b"unowned")
            .await
            .expect("outside");
        let bound = bind_ordinary_directory(&base.join(&key))
            .await
            .expect("bind")
            .expect("owned");
        tokio::fs::rename(&base, temp.path().join("held"))
            .await
            .expect("move parent");
        std::os::unix::fs::symlink(&outside, &base).expect("parent substitution");
        assert!(!remove_ordinary_directory(&bound)
            .await
            .expect("refuse parent"));
        assert!(outside.join(&key).join("keep").exists());
        assert!(temp
            .path()
            .join("held")
            .join(&key)
            .join("seg00000.m4s")
            .exists());
        let held = temp.path().join("held");
        let bound = bind_ordinary_directory(&held.join(&key))
            .await
            .expect("bind")
            .expect("owned");
        tokio::fs::rename(held.join(&key), held.join("original"))
            .await
            .expect("move child");
        tokio::fs::create_dir(held.join(&key))
            .await
            .expect("replacement");
        tokio::fs::write(held.join(&key).join("keep"), b"unowned")
            .await
            .expect("new data");
        assert!(!remove_ordinary_directory(&bound)
            .await
            .expect("refuse child"));
        assert!(held.join(&key).join("keep").exists());
        assert!(held.join("original/seg00000.m4s").exists());
    }
    use plurx_core::store::SqliteStore;

    #[tokio::test]
    async fn pressure_reclaims_restart_cold_media_but_preserves_private_ownership() {
        let temp = crate::test_tempdir().expect("cache");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(temp.path().to_path_buf(), store);
        let owner = VodCacheOwner(Arc::downgrade(&serve.shared));
        let ordinary = temp.path().join("a".repeat(64));
        let private = temp.path().join("b".repeat(64));
        for directory in [&ordinary, &private] {
            tokio::fs::create_dir(directory).await.expect("directory");
            tokio::fs::write(directory.join("init.mp4"), b"init")
                .await
                .expect("init");
            tokio::fs::write(directory.join("seg00000.m4s"), b"media")
                .await
                .expect("media");
        }
        tokio::fs::write(
            private.join(super::super::preparation_storage::MARKER),
            b"owner",
        )
        .await
        .expect("private marker");
        assert!(owner
            .evict(&CacheCandidate {
                path: ordinary.clone(),
                last_used: UNIX_EPOCH
            })
            .await
            .expect("cold eviction"));
        assert!(!ordinary.exists());
        assert!(!owner
            .evict(&CacheCandidate {
                path: private.clone(),
                last_used: UNIX_EPOCH
            })
            .await
            .expect("private preserved"));
        assert!(private.join("seg00000.m4s").exists());
    }

    #[tokio::test]
    async fn pressure_preserves_active_reader_then_retires_whole_admitted_rendition() {
        let temp = crate::test_tempdir().expect("cache");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = VodServe::new(temp.path().to_path_buf(), store);
        let owner = VodCacheOwner(Arc::downgrade(&serve.shared));
        let key = "c".repeat(64);
        let directory = temp.path().join(&key);
        let mut rendition = super::super::tests::synthetic_rendition(temp.path()).await;
        let inner = Arc::get_mut(&mut rendition).expect("unshared fixture");
        inner.key = key.clone();
        inner.dir = RenditionDir::new(&directory);
        inner.dir.create().await.expect("directory");
        inner.dir.write_init(b"init").await.expect("init");
        {
            let mut manifest = inner.manifest.lock().await;
            for index in 0..manifest.len() as u32 {
                inner
                    .dir
                    .materialize(&mut manifest, index, b"media", 1)
                    .await
                    .expect("media");
            }
            let bytes = manifest
                .complete(&Budgets {
                    working_set_bytes: 1 << 30,
                    admission_sizing_bytes: 1 << 30,
                    admission_share: 1.0,
                })
                .expect("admit");
            serve.shared.completed_cache.store(bytes, Relaxed);
        }
        rendition.attach_reader("viewer", 0).await;
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&rendition));
        let candidate = CacheCandidate {
            path: directory.clone(),
            last_used: UNIX_EPOCH,
        };
        assert!(!owner.evict(&candidate).await.expect("active protected"));
        assert!(directory.join("seg00000.m4s").exists());
        rendition
            .detach_media_reader(&serve.shared.pool, "viewer")
            .await;
        drop(rendition);
        assert!(owner.evict(&candidate).await.expect("idle eviction"));
        assert!(!directory.exists());
        assert!(!serve.shared.renditions.lock().await.contains_key(&key));
        assert_eq!(serve.shared.completed_cache.load(Relaxed), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pressure_refuses_symlinked_media() {
        let temp = crate::test_tempdir().expect("cache");
        let outside = temp.path().join("outside");
        tokio::fs::write(&outside, b"keep").await.expect("outside");
        let directory = temp.path().join("a".repeat(64));
        tokio::fs::create_dir(&directory).await.expect("directory");
        std::os::unix::fs::symlink(&outside, directory.join("seg00000.m4s")).expect("symlink");
        assert!(!ordinary_directory(&directory).await.expect("inventory"));
        assert_eq!(tokio::fs::read(outside).await.expect("preserved"), b"keep");
    }
}
