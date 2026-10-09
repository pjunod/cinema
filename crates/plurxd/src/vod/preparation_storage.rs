//! Storage ownership outlives the SQL job and every cancellable waiter.
use super::copy_preparation::PreparationAllowance;
use super::*;

pub(super) const MARKER: &str = ".preparation-owner.json";
const BATCH: usize = 128;
const MAX_DIRECTORIES: usize = 4096;
const MAX_FILES: usize = 65536;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    key: String,
    nonce: uuid::Uuid,
    cap: u64,
}

pub(super) struct ConstructionOwner(Option<Arc<PreparationAllowance>>);
impl ConstructionOwner {
    pub(super) fn new(storage: Arc<PreparationAllowance>) -> Self {
        Self(Some(storage))
    }
    pub(super) fn disarm(&mut self) {
        self.0.take();
    }
}
impl Drop for ConstructionOwner {
    fn drop(&mut self) {
        if let Some(storage) = self.0.take() {
            storage.release();
        }
    }
}

pub(super) struct Namespace {
    pub(super) key: String,
    pub(super) rendition: Weak<Rendition>,
    pub(super) directory_identity: Option<(u64, u64)>,
}

struct Cleanup {
    storage: Arc<PreparationAllowance>,
    created: Instant,
    next: Instant,
    failures: u32,
    failure: &'static str,
    running: bool,
}

#[derive(Clone)]
struct ColdFile {
    bytes: u64,
    identity: (u64, u64),
}
struct ColdDirectory {
    key: String,
    files: HashMap<PathBuf, ColdFile>,
    bytes: u64,
}
#[derive(Default)]
struct Inventory {
    root: Option<tokio::fs::ReadDir>,
    current: Option<(ColdDirectory, tokio::fs::ReadDir)>,
    deferred: Option<ColdDirectory>,
    cold: HashMap<String, ColdDirectory>,
    done: bool,
    blocked: bool,
    directories: usize,
    files: usize,
    validation_cursor: usize,
}

#[derive(Default)]
pub(super) struct StorageRegistry {
    inventory: Mutex<Inventory>,
    cleanup: StdMutex<HashMap<uuid::Uuid, Cleanup>>,
}

#[derive(Default, serde::Serialize)]
pub(crate) struct StorageDiagnostics {
    pub private_media_bytes: u64,
    pub pending_cleanup_count: usize,
    pub pending_cleanup_bytes: u64,
    pub oldest_cleanup_seconds: u64,
    pub last_failure_class: String,
    pub cold_unknown_bytes: u64,
    pub inventory_complete: bool,
}

impl StorageRegistry {
    pub(super) fn register(&self, storage: Arc<PreparationAllowance>) {
        self.cleanup
            .lock()
            .expect("private cleanup")
            .entry(storage.nonce)
            .or_insert_with(|| Cleanup {
                storage,
                created: Instant::now(),
                next: Instant::now(),
                failures: 0,
                failure: "none",
                running: false,
            });
    }

    /// One resumable metadata batch; unmarked cache is capacity, never demand.
    pub(super) async fn reconcile(&self, shared: &Arc<Shared>) {
        let shared = Arc::clone(shared);
        let receiver = spawn_cancellation_independent(async move {
            shared.preparation_storage.reconcile_owned(&shared).await;
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), receiver).await;
    }

    async fn reconcile_owned(&self, shared: &Arc<Shared>) {
        let Ok(mut inventory) = self.inventory.try_lock() else {
            return;
        };
        if inventory.blocked {
            return;
        }
        if inventory.done {
            self.revalidate_cold(shared, &mut inventory).await;
            return;
        }
        if !shared.retained_artifacts.own_namespace(&shared.base).await {
            return;
        }
        if inventory.root.is_none() {
            match tokio::fs::read_dir(&shared.base).await {
                Ok(root) => inventory.root = Some(root),
                Err(error) => {
                    inventory.blocked = true;
                    tracing::warn!(target:"plurxd::vodserve",%error,"private storage inventory blocked");
                    return;
                }
            }
        }
        for _ in 0..BATCH {
            if let Some(directory) = inventory.deferred.take() {
                match self
                    .reconcile_directory(shared, &mut inventory, directory)
                    .await
                {
                    Ok(Some(directory)) => {
                        inventory.deferred = Some(directory);
                        break;
                    }
                    Ok(None) => {}
                    Err(()) => {
                        inventory.blocked = true;
                        break;
                    }
                }
                continue;
            }
            if let Some((mut directory, mut entries)) = inventory.current.take() {
                shared
                    .hooks
                    .get()
                    .before_private_inventory_read("entry")
                    .await;
                match entries.next_entry().await {
                    Ok(Some(entry)) => {
                        inventory.files += 1;
                        if inventory.files > MAX_FILES {
                            inventory.blocked = true;
                            break;
                        }
                        let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                            Ok(metadata) if metadata.is_file() => metadata,
                            _ => {
                                inventory.blocked = true;
                                break;
                            }
                        };
                        let Some(total) = directory.bytes.checked_add(metadata.len()) else {
                            inventory.blocked = true;
                            break;
                        };
                        directory.bytes = total;
                        directory.files.insert(
                            entry.path(),
                            ColdFile {
                                bytes: metadata.len(),
                                identity: inode(&metadata),
                            },
                        );
                        inventory.current = Some((directory, entries));
                    }
                    Ok(None) => {
                        match self
                            .reconcile_directory(shared, &mut inventory, directory)
                            .await
                        {
                            Ok(Some(directory)) => {
                                inventory.deferred = Some(directory);
                                break;
                            }
                            Ok(None) => {}
                            Err(()) => {
                                inventory.blocked = true;
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        inventory.blocked = true;
                        break;
                    }
                }
                continue;
            }
            let next = inventory
                .root
                .as_mut()
                .expect("inventory root")
                .next_entry()
                .await;
            match next {
                Ok(Some(entry)) => {
                    let key = entry.file_name().to_string_lossy().into_owned();
                    if !managed_key(&key) {
                        continue;
                    }
                    // Namespace shape selects what to inventory, never what to delete.
                    let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                        Ok(metadata) if metadata.is_dir() => metadata,
                        _ => {
                            inventory.blocked = true;
                            break;
                        }
                    };
                    let _ = metadata;
                    inventory.directories += 1;
                    if inventory.directories > MAX_DIRECTORIES {
                        inventory.blocked = true;
                        break;
                    }
                    match tokio::fs::read_dir(entry.path()).await {
                        Ok(entries) => {
                            inventory.current = Some((
                                ColdDirectory {
                                    key,
                                    files: HashMap::new(),
                                    bytes: 0,
                                },
                                entries,
                            ))
                        }
                        Err(_) => {
                            inventory.blocked = true;
                            break;
                        }
                    }
                }
                Ok(None) => {
                    inventory.done = true;
                    break;
                }
                Err(_) => {
                    inventory.blocked = true;
                    break;
                }
            }
        }
        let cold = inventory
            .cold
            .values()
            .try_fold(0u64, |sum, directory| sum.checked_add(directory.bytes));
        shared.retained_artifacts.set_cold_capacity(
            cold.unwrap_or(u64::MAX),
            inventory.done && !inventory.blocked && cold.is_some(),
        );
        if inventory.blocked {
            tracing::warn!(target:"plurxd::vodserve","managed cache inventory incomplete; preparation admission closed");
        }
    }

    async fn reconcile_directory(
        &self,
        shared: &Arc<Shared>,
        inventory: &mut Inventory,
        directory: ColdDirectory,
    ) -> Result<Option<ColdDirectory>, ()> {
        let path = shared.base.join(&directory.key).join(MARKER);
        if directory.files.contains_key(&path) {
            shared
                .hooks
                .get()
                .before_private_inventory_read("marker")
                .await;
            let marker = read_marker(&path).await.map_err(|_| ())?;
            if marker.key != directory.key || marker.cap == 0 {
                return Err(());
            }
            let cap = marker.cap.max(directory.bytes);
            if !shared
                .retained_artifacts
                .restore_preparation(marker.nonce, cap)?
            {
                return Ok(Some(directory)); // Retry count pressure after cleanup drains.
            }
            let storage = Arc::new(PreparationAllowance::new(shared, marker.nonce, cap));
            storage.bind(&directory.key).map_err(|_| ())?;
            storage.restore_footprint(directory.bytes);
            storage.release();
        } else if directory.files.keys().any(|path| {
            path.file_name()
                .is_some_and(|name| name == ".preparation-owner.tmp")
        }) {
            // A torn marker is unknown ownership, never a licence to unlink.
            return Err(());
        } else {
            inventory.cold.insert(directory.key.clone(), directory);
        }
        Ok(None)
    }

    /// Transfer only exact inode/length matched, recipe-verified media.
    pub(super) async fn adopt(&self, shared: &Shared, rendition: &Rendition) {
        let mut inventory = self.inventory.lock().await;
        let Some(directory) = inventory.cold.get_mut(&rendition.key) else {
            return;
        };
        transfer_verified_media(directory, rendition).await;
        let cold = inventory
            .cold
            .values()
            .fold(0u64, |sum, directory| sum.saturating_add(directory.bytes));
        // Ordinary media is charged by the caller before admission can resume.
        shared
            .retained_artifacts
            .set_cold_capacity(cold, inventory.done && !inventory.blocked);
    }

    async fn revalidate_cold(&self, shared: &Shared, inventory: &mut Inventory) {
        let paths = inventory
            .cold
            .values()
            .flat_map(|directory| {
                directory
                    .files
                    .keys()
                    .map(|path| (directory.key.clone(), path.clone()))
            })
            .skip(inventory.validation_cursor)
            .take(BATCH)
            .collect::<Vec<_>>();
        inventory.validation_cursor = if paths.len() < BATCH {
            0
        } else {
            inventory.validation_cursor.saturating_add(paths.len())
        };
        let keys = paths
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();
        for key in keys {
            let Ok(_gate) = shared.rendition_build_gate(&key).try_lock_owned() else {
                continue;
            };
            let rendition = shared.renditions.lock().await.get(&key).cloned();
            if let Some(rendition) = rendition.filter(|rendition| {
                rendition.private_storage.is_none() && !rendition.closed.load(Acquire)
            }) {
                if let Some(directory) = inventory.cold.get_mut(&key) {
                    transfer_verified_media(directory, &rendition).await;
                }
            }
        }
        for (key, path) in paths {
            if matches!(tokio::fs::symlink_metadata(&path).await, Err(error) if error.kind() == io::ErrorKind::NotFound)
            {
                if let Some(directory) = inventory.cold.get_mut(&key) {
                    if let Some(file) = directory.files.remove(&path) {
                        directory.bytes = directory
                            .bytes
                            .checked_sub(file.bytes)
                            .expect("exact removed cold claim");
                    }
                }
            }
        }
        let cold = inventory
            .cold
            .values()
            .fold(0u64, |sum, directory| sum.saturating_add(directory.bytes));
        shared.retained_artifacts.set_cold_capacity(cold, true);
    }

    pub(super) async fn maintain(&self, shared: &Arc<Shared>) {
        let candidates = {
            let mut records = self.cleanup.lock().expect("private cleanup");
            let mut due = records
                .values_mut()
                .filter(|record| !record.running && record.next <= Instant::now())
                .collect::<Vec<_>>();
            due.sort_by_key(|record| record.next);
            due.into_iter()
                .take(8)
                .map(|record| {
                    record.running = true;
                    Arc::clone(&record.storage)
                })
                .collect::<Vec<_>>()
        };
        // Each attempt owns its exact key and survives cancellation of maintain.
        let mut tasks = Vec::new();
        for storage in candidates {
            let shared = Arc::clone(shared);
            tasks.push(tokio::spawn(async move {
                let result = cleanup_attempt(&shared, &storage).await;
                let mut records = shared.preparation_storage.cleanup.lock().expect("private cleanup");
                match result {
                    Ok(true) => { records.remove(&storage.nonce); }
                    result => if let Some(record) = records.get_mut(&storage.nonce) {
                        record.running = false;
                        if let Err(failure) = result {
                            record.failure = failure;
                            record.failures = record.failures.saturating_add(1);
                            tracing::warn!(target:"plurxd::vodserve", failure, "private preparation cleanup remains capacity-backed");
                        }
                        record.next = Instant::now() + Duration::from_secs(if record.failures <= 1 { 1 } else { (1u64 << record.failures.min(6)).min(60) });
                    },
                }
            }));
        }
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
    }

    pub(super) async fn diagnostics(&self, shared: &Shared) -> StorageDiagnostics {
        let records = self.cleanup.lock().expect("private cleanup");
        let snapshot = StorageDiagnostics {
            private_media_bytes: shared.preparation_media.load(Relaxed),
            pending_cleanup_count: records.len(),
            pending_cleanup_bytes: records.values().fold(0u64, |sum, record| {
                sum.saturating_add(record.storage.owned_bytes())
            }),
            oldest_cleanup_seconds: records
                .values()
                .map(|record| record.created.elapsed().as_secs())
                .max()
                .unwrap_or(0),
            last_failure_class: records
                .values()
                .find(|record| record.failure != "none")
                .map_or("none", |record| record.failure)
                .to_owned(),
            cold_unknown_bytes: shared.retained_artifacts.cold_capacity(),
            inventory_complete: shared.retained_artifacts.cold_ready(),
        };
        snapshot
    }
}

async fn cleanup_attempt(
    shared: &Arc<Shared>,
    storage: &Arc<PreparationAllowance>,
) -> Result<bool, &'static str> {
    let key = storage.key();
    let Some(key) = key else {
        return storage.finish().then_some(true).ok_or("writers");
    };
    let Ok(_gate) = shared.rendition_build_gate(&key).try_lock_owned() else {
        return Ok(false);
    };
    let current = shared.renditions.lock().await.get(&key).cloned();
    let owned = storage.rendition();
    if current.as_ref().is_some_and(|current| {
        !current
            .private_storage
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, storage))
    }) {
        return Err("incarnation");
    }
    match shared.store.quality_reserved_intervals(&key).await {
        Ok(intervals) if intervals.is_empty() => {}
        _ => return Err("dependencies"),
    }
    let rendition = owned.or(current);
    if let Some(rendition) = rendition.as_ref() {
        if !rendition.readers.lock().await.is_empty() {
            return Err("readers");
        }
        rendition.closed.store(true, Release);
        rendition.gen_epoch.fetch_add(1, AcqRel);
        rendition.kick();
        shared.pool.close(&key);
        if shared.hooks.get().private_cleanup_failure("termination") {
            return Err("termination");
        }
        match tokio::time::timeout(Duration::from_secs(2), rendition.slot.retire_storage()).await {
            Ok(Ok(())) => {}
            _ => return Err("termination"),
        }
    }
    if storage.inflight() != 0 {
        return Err("writers");
    }
    let directory = shared.base.join(&key);
    let directory_exists = match tokio::fs::symlink_metadata(&directory).await {
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Ok(metadata) if metadata.is_dir() => {
            match read_marker(&directory.join(MARKER)).await {
                Ok(marker) if marker.key == key && marker.nonce == storage.nonce => {
                    storage.directory_created(inode(&metadata));
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && storage.owns_directory(inode(&metadata)) => {}
                _ => return Err("incarnation"),
            }
            true
        }
        _ => return Err("namespace"),
    };
    // A replacement marker must preserve its Store row as well as its files.
    // The exact-key gate holds namespace identity through this deletion.
    if shared.hooks.get().private_cleanup_failure("store") {
        return Err("store");
    }
    shared
        .store
        .forget_rendition_plan(&key)
        .await
        .map_err(|_| "store")?;
    if directory_exists {
        if shared
            .hooks
            .get()
            .private_cleanup_failure_for("unlink", &key)
        {
            return Err("unlink");
        }
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(|_| "unlink")?;
        let mut removed = 0;
        while let Some(entry) = entries.next_entry().await.map_err(|_| "unlink")? {
            if entry.file_name() == MARKER {
                continue;
            }
            if removed == BATCH {
                return Ok(false);
            }
            if !tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|_| "unlink")?
                .is_file()
            {
                return Err("namespace");
            }
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|_| "unlink")?;
            if let (Some(rendition), Some(index)) = (
                rendition.as_ref(),
                planned_index(&entry.file_name().to_string_lossy()),
            ) {
                let mut manifest = rendition.manifest.lock().await;
                let bytes = manifest
                    .state(index)
                    .filter(|state| state.is_materialized())
                    .map_or(0, |state| state.bytes());
                if manifest.forget(index) && bytes > 0 {
                    storage.free_media(bytes);
                }
            }
            removed += 1;
        }
        match tokio::fs::remove_file(directory.join(MARKER)).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err("unlink"),
        }
        tokio::fs::remove_dir(&directory)
            .await
            .map_err(|_| "unlink")?;
    }
    if !storage.finish() {
        return Err("writers");
    }
    if let Some(rendition) = rendition {
        let mut renditions = shared.renditions.lock().await;
        if renditions
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &rendition))
        {
            renditions.remove(&key);
        }
    }
    shared.kick_all();
    Ok(true)
}

async fn transfer_verified_media(directory: &mut ColdDirectory, rendition: &Rendition) {
    let Ok(manifest) = rendition.manifest.try_lock() else {
        return;
    };
    for index in 0..rendition.plan.len() {
        let Some(state) = manifest
            .state(index as u32)
            .filter(|state| state.is_materialized())
        else {
            continue;
        };
        let path = rendition.dir.path().join(segment_name(index as u64));
        let Some(file) = directory.files.get(&path) else {
            continue;
        };
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !metadata.is_file()
            || file.identity == (0, 0)
            || inode(&metadata) != file.identity
            || metadata.len() != file.bytes
            || state.bytes() != file.bytes
        {
            continue; // Unknown ownership stays conservatively reserved.
        }
        let bytes = file.bytes;
        directory.files.remove(&path);
        directory.bytes = directory
            .bytes
            .checked_sub(bytes)
            .expect("exact cold ownership");
    }
}

fn managed_key(key: &str) -> bool {
    let hash = key.strip_prefix("source-").unwrap_or(key);
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}
#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

pub(super) async fn has_private_marker(directory: &Path) -> io::Result<bool> {
    for name in [MARKER, ".preparation-owner.tmp"] {
        match tokio::fs::symlink_metadata(directory.join(name)).await {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

async fn read_marker(path: &Path) -> io::Result<Marker> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let marker: Marker =
        serde_json::from_slice(&tokio::fs::read(path).await?).map_err(io::Error::other)?;
    if marker.version != 1 || !managed_key(&marker.key) || marker.cap == 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(marker)
}

impl PreparationAllowance {
    pub(super) async fn create_marker(self: &Arc<Self>, dir: &RenditionDir) -> Result<(), String> {
        if let Some(shared) = self.shared.upgrade() {
            shared.hooks.get().before_private_marker().await;
        }
        let marker = Marker {
            version: 1,
            key: self.key().ok_or("unbound storage")?,
            nonce: self.nonce,
            cap: self.cap,
        };
        let bytes = serde_json::to_vec(&marker).map_err(|error| error.to_string())?;
        let pending = self
            .begin(
                (bytes.len() as u64)
                    .checked_mul(2)
                    .ok_or("marker overflow")?,
            )
            .ok_or("marker exceeds preparation cap")?;
        dir.create().await.map_err(|error| error.to_string())?;
        self.directory_created(inode(
            &tokio::fs::symlink_metadata(dir.path())
                .await
                .map_err(|error| error.to_string())?,
        ));
        let temporary = dir.path().join(".preparation-owner.tmp");
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|error| error.to_string())?;
        sync_file(&temporary)
            .await
            .map_err(|error| error.to_string())?;
        tokio::fs::rename(temporary, dir.path().join(MARKER))
            .await
            .map_err(|error| error.to_string())?;
        sync_file(dir.path())
            .await
            .map_err(|error| error.to_string())?;
        pending.commit(false);
        Ok(())
    }
}

impl Rendition {
    pub(super) fn free_unadmitted(&self, shared: &Shared, bytes: u64) {
        if let Some(storage) = self.private_storage.as_ref() {
            storage.free_media(bytes);
        } else {
            sub_saturating(&shared.working_set, bytes);
        }
    }
}

impl VodServe {
    pub(crate) async fn preparation_storage_diagnostics(&self) -> StorageDiagnostics {
        self.shared
            .preparation_storage
            .diagnostics(&self.shared)
            .await
    }
}
