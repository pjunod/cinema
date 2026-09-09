//! Keeping the pre-transcode cache inside its budget, and cleaning up after
//! producers that died.
//!
//! Separate from the producer because it has to run whether or not anything is
//! being produced. A cache that only sweeps when it is also filling is a cache
//! that holds its high-water mark forever once you turn production off — and
//! turning production off is exactly what somebody does when their disk is
//! full.
//!
//! Three jobs, in the order they must happen:
//!
//! 1. **Crash leftovers.** A `complete = 0` row is either a producer at work or
//!    a producer that died; only age tells them apart. Old ones are swept
//!    first because their bytes are on the disk the budget is about.
//! 2. **Budget.** Evict coldest-first until the total fits. LRU by
//!    `last_used_at`, which the serving path touches on every hit — so what
//!    survives is what people come back to, not what happened to be made last.
//! 3. **Orphan directories.** Bytes on disk with no row are invisible to every
//!    query above, which makes them the one kind of leak a budget cannot
//!    correct: the sum says there is room and the filesystem disagrees.
//!
//! Deleting the directory before the row, everywhere. The other order leaves a
//! row pointing at nothing when the process dies between the two, and that row
//! is a *hit* — a viewer gets a playlist for a directory that no longer exists.
//! This way the failure is an orphan directory, which step 3 collects.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use plurx_core::domain::{CacheManifestCheck, CachedTranscode};
use plurx_core::store::{keys, Store};

/// Process-local ownership for finished cache entries that are being served.
///
/// The store records whether bytes exist; it cannot record whether an HTTP
/// response on this node is reading them right now. Readers claim a recipe
/// before looking it up and keep that claim for the session's lifetime.
/// Eviction claims the same recipe exclusively before deleting anything, so
/// lookup and removal cannot pass each other between the row check and the
/// filesystem operation.
///
/// The key is deliberately the recipe hash rather than
/// `(recipe_hash, node_id, storage_class)`: one manager owns one node process,
/// and a reader of any copy must conservatively block removal of every copy
/// that process could resolve. The safe cost is temporary over-protection;
/// including a class here would let a future routing mismatch under-protect
/// bytes that are actually in use.
#[derive(Clone, Default)]
pub struct ActiveCacheReaders {
    states: Arc<Mutex<HashMap<String, CacheActivity>>>,
    active_entries: Arc<AtomicUsize>,
    orphan_walker: Arc<tokio::sync::Mutex<CacheOrphanWalker>>,
    sweep: Arc<tokio::sync::Mutex<()>>,
}

/// Read-only, lock-free cache activity projection for process metrics.
///
/// This deliberately carries only the published counter, not the ownership
/// map or any of the Store-bearing housekeeping machinery.
#[derive(Clone)]
pub(crate) struct ActiveCacheMetrics {
    active_entries: Arc<AtomicUsize>,
}

impl ActiveCacheMetrics {
    pub(crate) fn active_entries(&self) -> usize {
        self.active_entries.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct CacheOrphanWalker {
    root: Option<PathBuf>,
    prefixes: Option<tokio::fs::ReadDir>,
    final_entries: Option<(PathBuf, tokio::fs::ReadDir)>,
    final_resume: Option<PathBuf>,
    staging_entries: Option<tokio::fs::ReadDir>,
}

#[derive(Clone, Copy)]
enum CacheActivity {
    Readers { count: usize, playback_count: usize },
    Evicting,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CacheBusy {
    Readers,
    Evicting,
}

pub struct CacheReadGuard {
    readers: ActiveCacheReaders,
    recipe: String,
    playback: bool,
}

/// Two-dimensional ownership for queue publication. Budget eviction keys by
/// recipe while orphan cleanup keys by the final directory name; both guards
/// must span rename through fenced completion.
pub(crate) struct CachePublicationGuard {
    _recipe: CacheReadGuard,
    _final_directory: CacheReadGuard,
    _parent_directory: CacheReadGuard,
}

/// A staging producer protects both its recipe-specific child and the shared
/// `tmp` parent. Orphan cleanup is allowed to remove empty shared parents, but
/// never between a producer's parent check and child installation.
pub(crate) struct CacheStagingGuard {
    _staging_directory: CacheReadGuard,
    _parent_directory: CacheReadGuard,
}

pub(crate) struct CacheEvictionGuard {
    readers: ActiveCacheReaders,
    recipe: String,
}

impl ActiveCacheReaders {
    pub(crate) fn metrics(&self) -> ActiveCacheMetrics {
        ActiveCacheMetrics {
            active_entries: Arc::clone(&self.active_entries),
        }
    }

    fn lock_states(&self) -> MutexGuard<'_, HashMap<String, CacheActivity>> {
        self.states.lock().unwrap_or_else(|poisoned| {
            tracing::error!("cache ownership mutex was poisoned; recovering its state");
            poisoned.into_inner()
        })
    }

    /// Hold one recipe for an internal scrub, offline transfer, or queue
    /// reuse, unless eviction already owns it. These are readers for deletion
    /// safety but are not live playback sessions in operational metrics.
    pub fn begin_read(&self, recipe: &str) -> Option<CacheReadGuard> {
        self.begin_guard(recipe, false)
    }

    /// Hold a cached generation for one live playback session.
    pub(crate) fn begin_playback(&self, recipe: &str) -> Option<CacheReadGuard> {
        self.begin_guard(recipe, true)
    }

    /// Protect a lookup without claiming that a miss was ever a real recipe.
    pub(crate) fn begin_lookup(&self, recipe: &str) -> Option<CacheReadGuard> {
        self.begin_guard(recipe, false)
    }

    fn begin_guard(&self, recipe: &str, playback: bool) -> Option<CacheReadGuard> {
        let mut states = self.lock_states();
        match states.get_mut(recipe) {
            Some(CacheActivity::Readers {
                count,
                playback_count,
            }) => {
                *count += 1;
                if playback {
                    if *playback_count == 0 {
                        self.active_entries.fetch_add(1, Ordering::Relaxed);
                    }
                    *playback_count += 1;
                }
            }
            Some(CacheActivity::Evicting) => return None,
            None => {
                states.insert(
                    recipe.to_owned(),
                    CacheActivity::Readers {
                        count: 1,
                        playback_count: usize::from(playback),
                    },
                );
                if playback {
                    self.active_entries.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Some(CacheReadGuard {
            readers: self.clone(),
            recipe: recipe.to_owned(),
            playback,
        })
    }

    /// Block both eviction of an older location for this recipe and orphan
    /// removal of the just-renamed generation until its durable row exists.
    pub(crate) fn begin_publication(
        &self,
        recipe: &str,
        final_directory: &str,
    ) -> Option<CachePublicationGuard> {
        let parent = recipe.get(..2)?;
        if !final_directory.starts_with(parent) {
            return None;
        }
        let recipe = self.begin_guard(recipe, false)?;
        let final_directory = self.begin_guard(final_directory, false)?;
        let parent_directory = self.begin_guard(&cache_parent_key(parent), false)?;
        Some(CachePublicationGuard {
            _recipe: recipe,
            _final_directory: final_directory,
            _parent_directory: parent_directory,
        })
    }

    /// Exclude orphan cleanup from a producer's resumable directory. Fenced
    /// producers key staging by canonical recipe; queue producers key it by
    /// job-stable directory name, matching the orphan walk below.
    pub(crate) fn begin_staging(&self, staging_directory: &str) -> Option<CacheStagingGuard> {
        let staging_directory = self.begin_guard(staging_recipe(staging_directory), false)?;
        let parent_directory = self.begin_guard(&cache_parent_key(STAGING), false)?;
        Some(CacheStagingGuard {
            _staging_directory: staging_directory,
            _parent_directory: parent_directory,
        })
    }

    /// Claim a recipe for removal. Active readers make eviction skip it; the
    /// next sweep will retry after their sessions end.
    pub(crate) fn begin_eviction(&self, recipe: &str) -> Result<CacheEvictionGuard, CacheBusy> {
        let mut states = self.lock_states();
        match states.get(recipe) {
            Some(CacheActivity::Readers { .. }) => return Err(CacheBusy::Readers),
            Some(CacheActivity::Evicting) => return Err(CacheBusy::Evicting),
            None => {}
        }
        states.insert(recipe.to_owned(), CacheActivity::Evicting);
        Ok(CacheEvictionGuard {
            readers: self.clone(),
            recipe: recipe.to_owned(),
        })
    }

    /// Number of recipes protected by at least one active playback session.
    /// Reader multiplicity stays internal; the operational question is how
    /// many cache entries housekeeping is presently forbidden to remove.
    #[cfg(test)]
    pub fn active_entries(&self) -> usize {
        self.active_entries.load(Ordering::Relaxed)
    }

    /// Whether anything except one housekeeping read owns this recipe.
    /// Unrelated foreground playback must not starve integrity work for the
    /// rest of the cache; the physical and wall-clock budgets bound aggregate
    /// background pressure instead.
    fn has_readers_besides(&self, recipe: &str) -> bool {
        matches!(
            self.lock_states().get(recipe),
            Some(CacheActivity::Readers { count, .. }) if *count > 1
        )
    }

    fn has_reader(&self, recipe: &str) -> bool {
        matches!(
            self.lock_states().get(recipe),
            Some(CacheActivity::Readers { .. })
        )
    }
}

impl Drop for CacheReadGuard {
    fn drop(&mut self) {
        let mismatch = {
            let mut states = self.readers.lock_states();
            let mut playback_became_inactive = false;
            let action = match states.get_mut(&self.recipe) {
                Some(CacheActivity::Readers {
                    count,
                    playback_count,
                }) if *count > 0 && (!self.playback || *playback_count > 0) => {
                    *count -= 1;
                    if self.playback {
                        *playback_count -= 1;
                        playback_became_inactive = *playback_count == 0;
                    }
                    usize::from(*count == 0)
                }
                Some(CacheActivity::Readers { .. } | CacheActivity::Evicting) | None => 2,
            };
            if action == 1 {
                states.remove(&self.recipe);
            }
            if playback_became_inactive {
                self.readers.active_entries.fetch_sub(1, Ordering::Relaxed);
            }
            action == 2
        };
        if mismatch {
            tracing::error!(
                recipe = %self.recipe,
                "cache reader ownership changed while its guard was held"
            );
        }
    }
}

impl Drop for CacheEvictionGuard {
    fn drop(&mut self) {
        let mismatch = {
            let mut states = self.readers.lock_states();
            if matches!(states.get(&self.recipe), Some(CacheActivity::Evicting)) {
                states.remove(&self.recipe);
                false
            } else {
                true
            }
        };
        if mismatch {
            tracing::error!(
                recipe = %self.recipe,
                "cache eviction ownership changed while its guard was held"
            );
        }
    }
}

/// How old an unfinished claim has to be before it counts as a crash rather
/// than as work in progress.
///
/// A day, deliberately generously. The cost of waiting is one stale directory;
/// the cost of being wrong in the other direction is deleting the output of a
/// producer that is still writing it — a 4K film can take hours, and a job
/// paused behind live playback for an afternoon is normal, not dead.
pub const STALE_CLAIM_SECS: i64 = 24 * 3600;

/// Default budget when nothing is set. Fifty gigabytes is a few 4K films: big
/// enough that Next Up is nearly always warm, small enough to be an obviously
/// safe default on a NAS.
pub const DEFAULT_MAX_GB: i64 = 50;

/// A sweep advances only this many generation locations. Successful pages
/// update `last_seen_at`, so the oldest-first query rotates across the full
/// inventory without an unbounded filesystem walk.
const MANIFEST_SCRUB_BATCH: i64 = 128;

/// CPU/syscall ceiling for degenerate manifests containing many tiny objects.
/// The byte and wall-clock budgets usually stop the scrub first.
const MANIFEST_SCRUB_MAX_OBJECTS: usize = 4_096;
const MANIFEST_SCRUB_MAX_WALL: std::time::Duration = std::time::Duration::from_secs(2);

/// Physical I/O ceiling for one cleanup pass. The manifest format rejects any
/// individual object above this same bound, so the first object can never
/// punch through it.
// One maximally valid object must still fit after loading the maximally valid
// manifest and reserving both EOF probes. Otherwise the oldest row can pin the
// durable cursor forever and starve every later generation.
const MANIFEST_SCRUB_BYTES: u64 = plurx_core::transcode::manifest::MAX_OBJECT_BYTES
    + plurx_core::transcode::manifest::MAX_MANIFEST_BYTES
    + 2;

fn reserve_scrub_bytes(remaining: &mut u64, bytes: u64) -> bool {
    if bytes > *remaining {
        return false;
    }
    *remaining -= bytes;
    true
}

/// Where a producer assembles an entry before publishing it: one directory per
/// recipe, under the cache root so the publish is a rename on one filesystem.
///
/// Named here rather than in the producer because *this* file is the one that
/// deletes things. It sits at the same depth as a fanout prefix and looks
/// exactly like one, so the orphan walk would otherwise treat a producer's
/// half-built asset as a leftover and remove it mid-encode — a producer losing
/// hours of work to the housekeeping job that runs beside it.
pub const STAGING: &str = "tmp";

fn cache_parent_key(name: &str) -> String {
    format!("cache-parent:{name}")
}

/// The staging directory for one recipe. Deterministic, so a later pass can
/// find what an earlier one left and resume from it.
pub fn staging_dir(root: &Path, recipe_hash: &str) -> PathBuf {
    root.join(STAGING).join(recipe_hash)
}

/// P2's cluster-owned producer isolates an attempt by lease fence while the
/// durable claim remains keyed by the canonical recipe. The orphan sweep must
/// therefore protect `HASH-fN` staging whenever `HASH` is still claimed.
fn staging_recipe(name: &str) -> &str {
    name.rsplit_once("-f")
        .filter(|(recipe, fence)| {
            !recipe.is_empty()
                && !fence.is_empty()
                && fence.bytes().all(|byte| byte.is_ascii_digit())
        })
        .map_or(name, |(recipe, _)| recipe)
}

/// Distributed queue staging is kept by job id instead of an incomplete
/// cache location. The recipe prefix still ensures one encoder identity per
/// directory; the job suffix is the durable housekeeping authority.
fn staging_queue_job(name: &str) -> Option<&str> {
    let (recipe, job_id) = name.rsplit_once("-j")?;
    if recipe.len() != 64 || !recipe.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    uuid::Uuid::parse_str(job_id).ok().map(|_| job_id)
}

fn staging_queue_recipe(name: &str) -> Option<&str> {
    let (recipe, job_id) = name.rsplit_once("-j")?;
    (recipe.len() == 64
        && recipe.bytes().all(|byte| byte.is_ascii_hexdigit())
        && uuid::Uuid::parse_str(job_id).is_ok())
    .then_some(recipe)
}

/// A queue generation renamed into its final fanout directory but not yet
/// bound by the fenced transaction. This exact syntax lets the durable queue
/// inventory authorize cleanup even when the cache-location inventory is
/// legitimately empty after a restart.
fn final_queue_job(name: &str) -> Option<&str> {
    final_queue_generation(name).map(|(job, _)| job)
}

fn final_queue_generation(name: &str) -> Option<(&str, i64)> {
    let (staging_name, fence) = name.rsplit_once("-f")?;
    let fence = (!fence.is_empty() && fence.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| fence.parse::<i64>().ok())
        .flatten()?;
    Some((staging_queue_job(staging_name)?, fence))
}

fn final_recipe(name: &str) -> Option<&str> {
    if final_queue_job(name).is_some() {
        return name
            .rsplit_once("-f")
            .and_then(|(staging, _)| staging_queue_recipe(staging));
    }
    let recipe = staging_recipe(name);
    (recipe.len() == 64 && recipe.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(recipe)
}

const ORPHAN_RECHECK_BATCH: usize = 128;
const ORPHAN_SCAN_LIMIT: usize = 10_000;
const ORPHAN_DELETE_LIMIT: usize = 256;

struct OrphanCandidate {
    path: PathBuf,
    parent: PathBuf,
    name: String,
    identity: plurx_core::fs_secure::FileIdentity,
    max_depth: usize,
    _eviction: Option<CacheEvictionGuard>,
}

const MAX_FLAT_CACHE_CHILDREN: usize = crate::transcode::MAX_PRETRANSCODE_CLEANUP_ENTRIES;

async fn quarantine_and_remove(candidate: &OrphanCandidate) -> bool {
    let quarantine = format!(".plurx-delete-{}", uuid::Uuid::new_v4().simple());
    if let Err(error) =
        plurx_core::fs_secure::rename_child(&candidate.parent, &candidate.name, &quarantine).await
    {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(dir = %candidate.path.display(), %error, "cache: could not quarantine directory");
        }
        return error.kind() == std::io::ErrorKind::NotFound;
    }

    let quarantined_path = candidate.parent.join(&quarantine);
    let identity = plurx_core::fs_secure::directory_identity_nofollow(&quarantined_path).await;
    if !matches!(&identity, Ok(found) if found.same_inode(candidate.identity)) {
        let restored = plurx_core::fs_secure::rename_child_noreplace(
            &candidate.parent,
            &quarantine,
            &candidate.name,
        )
        .await;
        tracing::warn!(
            dir = %candidate.path.display(),
            ?identity,
            ?restored,
            "cache: quarantined directory identity changed; preserving bytes"
        );
        return false;
    }

    if candidate.max_depth > 0 {
        let staging = match plurx_core::fs_secure::SecureDirectory::open(&quarantined_path).await {
            Ok(staging) => staging,
            Err(error) => {
                let _ = plurx_core::fs_secure::rename_child_noreplace(
                    &candidate.parent,
                    &quarantine,
                    &candidate.name,
                )
                .await;
                tracing::warn!(dir = %candidate.path.display(), %error, "cache: could not bind quarantined staging tree");
                return false;
            }
        };
        // A crash may leave a generation-sized assembly alongside the maximum
        // resumable parts tree. Remove those known children under the held
        // staging capability before applying the independent parts bound.
        for child in [
            crate::transcode::ASSEMBLED_TEMP_DIR,
            crate::transcode::ASSEMBLED_DIR,
        ] {
            match staging
                .remove_child_tree(child, plurx_core::transcode::manifest::MAX_OBJECTS + 100, 2)
                .await
            {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let _ = plurx_core::fs_secure::rename_child_noreplace(
                        &candidate.parent,
                        &quarantine,
                        &candidate.name,
                    )
                    .await;
                    tracing::warn!(dir = %candidate.path.display(), child, %error, "cache: bounded assembly cleanup failed; preserving staging bytes");
                    return false;
                }
            }
        }
    }

    let removed = if candidate.max_depth == 0 {
        plurx_core::fs_secure::remove_flat_directory_child(
            &candidate.parent,
            &quarantine,
            MAX_FLAT_CACHE_CHILDREN,
        )
        .await
    } else {
        plurx_core::fs_secure::remove_bounded_directory_tree_child(
            &candidate.parent,
            &quarantine,
            MAX_FLAT_CACHE_CHILDREN,
            candidate.max_depth,
        )
        .await
    };
    match removed {
        Ok(()) => true,
        Err(error) => {
            let restored = plurx_core::fs_secure::rename_child_noreplace(
                &candidate.parent,
                &quarantine,
                &candidate.name,
            )
            .await;
            tracing::warn!(
                dir = %candidate.path.display(),
                %error,
                ?restored,
                "cache: bounded quarantine removal failed; preserving bytes"
            );
            false
        }
    }
}

struct OwnershipSnapshot {
    paths: HashSet<PathBuf>,
    claimed_recipes: HashSet<String>,
    queue_jobs: HashSet<String>,
    has_owners: bool,
    complete: bool,
}

/// A complete inventory is authoritative even when it is empty. An
/// incomplete inventory may still protect and recheck positive owners, but an
/// incomplete empty result contains no fact that authorizes filesystem
/// deletion and must fail closed.
fn orphan_inventory_authorized(snapshot: &OwnershipSnapshot) -> bool {
    snapshot.complete || snapshot.has_owners
}

async fn ownership_snapshot(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
) -> Result<OwnershipSnapshot, plurx_core::error::StoreError> {
    let inventory = store.cache_ownership_inventory(node_id).await?;
    let queue_jobs = store
        .pretranscode_staging_jobs(node_id)
        .await?
        .into_iter()
        .collect::<HashSet<_>>();
    let local_rows = inventory
        .rows
        .into_iter()
        .filter(|entry| entry.storage_class == "local")
        .collect::<Vec<_>>();
    let has_owners = !local_rows.is_empty() || !queue_jobs.is_empty();
    let mut paths = HashSet::new();
    for entry in &local_rows {
        if let Some(path) = validated_entry_dir(root, &entry.relative_dir).await {
            paths.insert(path);
        }
    }
    let claimed_recipes = local_rows
        .into_iter()
        .filter(|entry| !entry.complete)
        .map(|entry| entry.recipe_hash)
        .collect();
    Ok(OwnershipSnapshot {
        paths,
        claimed_recipes,
        queue_jobs,
        has_owners,
        complete: inventory.complete,
    })
}

async fn remove_empty_cache_parent(readers: &ActiveCacheReaders, root: &Path, parent: &Path) {
    if parent.parent() != Some(root) {
        return;
    }
    let Some(name) = parent.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Ok(_parent_eviction) = readers.begin_eviction(&cache_parent_key(name)) else {
        // A producer acquires this shared-parent read guard before ensuring the
        // directory exists and holds it until its child has been installed.
        return;
    };
    if let Err(error) = plurx_core::fs_secure::remove_empty_directory_child(root, name).await {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
        ) {
            tracing::warn!(dir = %parent.display(), %error, "cache: could not remove empty cache parent");
        }
    }
}

async fn delete_final_batch(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
    readers: &ActiveCacheReaders,
    batch: &mut Vec<OrphanCandidate>,
) -> (usize, usize) {
    let relative_dirs = batch
        .iter()
        .filter_map(|candidate| candidate.path.strip_prefix(root).ok())
        .filter_map(|relative| relative.to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let rows = match store
        .cache_candidate_owners(node_id, &relative_dirs, &[])
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "cache: could not recheck final-directory ownership; keeping bytes");
            let kept = batch.len();
            batch.clear();
            return (0, kept);
        }
    };
    let mut owned_paths = HashSet::new();
    for entry in rows {
        if let Some(path) = validated_entry_dir(root, &entry.relative_dir).await {
            owned_paths.insert(path);
        }
    }
    let queue_jobs = match store.pretranscode_staging_jobs(node_id).await {
        Ok(jobs) => jobs.into_iter().collect::<HashSet<_>>(),
        Err(error) => {
            tracing::warn!(%error, "cache: could not recheck queue ownership; keeping bytes");
            let kept = batch.len();
            batch.clear();
            return (0, kept);
        }
    };
    let mut removed = 0;
    let mut kept = 0;
    // Iterate by reference so every unique-key eviction guard in the batch
    // remains alive until every same-recipe generation has been decided.
    for candidate in batch.iter() {
        let queue_owned = if let Some((job_id, fence)) = final_queue_generation(&candidate.name) {
            if !queue_jobs.contains(job_id) {
                false
            } else {
                match store.pretranscode_job(job_id).await {
                    Ok(Some(job)) => {
                        job.state == "running" && job.owner_node_id == node_id && job.fence == fence
                    }
                    Ok(None) => false,
                    Err(error) => {
                        tracing::warn!(job = job_id, %error, "cache: could not recheck exact queue generation; keeping bytes");
                        true
                    }
                }
            }
        } else {
            false
        };
        let owned = owned_paths.contains(&candidate.path) || queue_owned;
        if owned {
            kept += 1;
            continue;
        }
        if quarantine_and_remove(candidate).await {
            removed += 1;
            tracing::info!(dir = %candidate.path.display(), "cache: removed an unclaimed directory");
            remove_empty_cache_parent(readers, root, &candidate.parent).await;
        } else {
            kept += 1;
        }
    }
    batch.clear();
    (removed, kept)
}

async fn delete_staging_batch(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
    readers: &ActiveCacheReaders,
    batch: &mut Vec<OrphanCandidate>,
) -> (usize, usize) {
    let recipes = batch
        .iter()
        .map(|candidate| staging_recipe(&candidate.name).to_owned())
        .collect::<Vec<_>>();
    let rows = match store.cache_candidate_owners(node_id, &[], &recipes).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "cache: could not recheck staging ownership; keeping bytes");
            let kept = batch.len();
            batch.clear();
            return (0, kept);
        }
    };
    let claimed_recipes = rows
        .into_iter()
        .filter(|entry| !entry.complete)
        .map(|entry| entry.recipe_hash)
        .collect::<HashSet<_>>();
    let queue_jobs = match store.pretranscode_staging_jobs(node_id).await {
        Ok(jobs) => jobs.into_iter().collect::<HashSet<_>>(),
        Err(error) => {
            tracing::warn!(%error, "cache: could not recheck queue ownership; keeping bytes");
            let kept = batch.len();
            batch.clear();
            return (0, kept);
        }
    };
    let mut removed = 0;
    let mut kept = 0;
    // Same-recipe fenced staging directories share one canonical guard; keep
    // it until the whole batch is complete.
    for candidate in batch.iter() {
        let owned = staging_queue_job(&candidate.name).is_some_and(|job| queue_jobs.contains(job))
            || claimed_recipes.contains(staging_recipe(&candidate.name));
        if owned {
            kept += 1;
            continue;
        }
        if quarantine_and_remove(candidate).await {
            removed += 1;
            tracing::info!(dir = %candidate.path.display(), "cache: removed staging for a recipe nothing claims");
            remove_empty_cache_parent(readers, root, &candidate.parent).await;
        } else {
            kept += 1;
        }
    }
    batch.clear();
    (removed, kept)
}

const GB: i64 = 1024 * 1024 * 1024;

/// What one sweep did, for the log and for tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    /// Unfinished claims old enough to be crash leftovers.
    pub stale: usize,
    /// Complete entries evicted to get under budget.
    pub evicted: usize,
    /// Manifest-fenced locations invalidated before they could be offered.
    pub corrupt: usize,
    /// Complete entries skipped because an active session is reading them.
    pub protected: usize,
    /// Entries already owned by another sweep in this process.
    pub in_flight: usize,
    /// Directories on disk that no row claimed.
    pub orphans: usize,
    /// Bounded ownership snapshots taken after candidate eviction guards.
    pub ownership_rechecks: usize,
    pub bytes_freed: i64,
    /// Conservative manifest + media bytes admitted to the integrity scrub.
    pub scrub_bytes: u64,
    /// What the cache occupies now, by the rows.
    pub bytes_after: i64,
}

/// The cache's disk budget in bytes, or `None` when the cache is switched off.
///
/// Off is a real answer rather than a budget of zero, and the difference shows
/// up one line later: a zero budget evicts everything on every sweep, which is
/// what somebody who set `max_gb = 0` to stop the cache actually wants.
pub async fn budget_bytes(store: &Arc<dyn Store>) -> Option<i64> {
    budget_bytes_fallible(store)
        .await
        .unwrap_or(Some(DEFAULT_MAX_GB * GB))
}

pub async fn budget_bytes_fallible(
    store: &Arc<dyn Store>,
) -> Result<Option<i64>, plurx_core::error::StoreError> {
    let gb = match store.get_setting(keys::CACHE_MAX_GB).await? {
        Some(value) => value
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
            .ok_or_else(|| {
                plurx_core::error::StoreError::Task(
                    "cache budget setting is not a non-negative integer".to_owned(),
                )
            })?,
        None => DEFAULT_MAX_GB,
    };
    Ok((gb > 0).then(|| gb.saturating_mul(GB)))
}

/// Run the whole sweep. Safe to call at any time; does nothing surprising when
/// the cache is empty, unconfigured, or its root does not exist.
///
/// `now` is passed in rather than read here, for the same reason
/// [`crate::schedule::due_jobs`] takes one: the interesting cases are all about
/// elapsed time, and a function that reads its own clock can only be tested by
/// waiting. Here that would mean waiting a day.
pub async fn sweep_with_readers(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
    readers: &ActiveCacheReaders,
    now: i64,
) -> Swept {
    let Ok(_sweep) = readers.sweep.try_lock() else {
        tracing::debug!(node_id, "cache: coalesced an overlapping local sweep");
        return Swept::default();
    };
    let mut out = Swept::default();

    // 1. Crash leftovers, before the budget: their bytes are on the same disk.
    match store
        .stale_cache_claims(node_id, now - STALE_CLAIM_SECS)
        .await
    {
        Ok(stale) => {
            for entry in stale {
                let _eviction = match readers.begin_eviction(&entry.recipe_hash) {
                    Ok(guard) => guard,
                    Err(CacheBusy::Readers) => {
                        out.protected += 1;
                        continue;
                    }
                    Err(CacheBusy::Evicting) => {
                        out.in_flight += 1;
                        continue;
                    }
                };
                if forget(store, root, node_id, &entry).await {
                    out.stale += 1;
                    // Not added to `bytes_freed`: an incomplete row's `bytes`
                    // is whatever it was when it was claimed, which is zero.
                    // Reporting it would make the figure a fiction.
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cache: could not list stale claims"),
    }

    // 2. Authenticate a bounded rotating page of generations. Each location
    // advances through a bounded object page as it rotates through the
    // oldest-first durable cursor; requested objects are also verified on the
    // serving path. Invalidating the exact row first makes a crash leave an
    // unowned directory, never a durable hit for missing bytes.
    let mut manifest_checks = Vec::new();
    let mut scrub_remaining = MANIFEST_SCRUB_BYTES;
    let scrub_started = std::time::Instant::now();
    let mut scrubbed_objects = 0usize;
    match store
        .cache_manifest_candidates(node_id, MANIFEST_SCRUB_BATCH)
        .await
    {
        Ok(candidates) => {
            for entry in candidates {
                let Some(expected_digest) = entry.manifest_digest.as_deref() else {
                    continue;
                };
                // Playback itself refreshes last_used_at, so skipping the same
                // recipe does not age a valid ready holder out. Unrelated
                // playback does not suppress this location's cheap heartbeat.
                if readers.has_reader(&entry.recipe_hash) {
                    continue;
                }
                // A shared read excludes eviction but allows playback to
                // start. Foreground readers are detected between objects,
                // which bounds their worst-case wait to one HLS object.
                let Some(_scrub_reader) = readers.begin_read(&entry.recipe_hash) else {
                    out.in_flight += 1;
                    continue;
                };
                let directory = validated_entry_dir(root, &entry.relative_dir).await;
                let manifest_present = match directory.as_deref() {
                    Some(directory) => {
                        match plurx_core::fs_secure::open_read_nofollow(
                            &directory.join(plurx_core::transcode::manifest::MANIFEST_FILE),
                        )
                        .await
                        {
                            Ok(file) => file.metadata().await.is_ok_and(|metadata| {
                                metadata.is_file()
                                    && metadata.len() > 0
                                    && metadata.len()
                                        <= plurx_core::transcode::manifest::MAX_MANIFEST_BYTES
                            }),
                            Err(_) => false,
                        }
                    }
                    None => false,
                };
                let deep_allowed = manifest_present
                    && scrub_remaining > 0
                    && scrubbed_objects < MANIFEST_SCRUB_MAX_OBJECTS
                    && scrub_started.elapsed() < MANIFEST_SCRUB_MAX_WALL;
                let manifest = if deep_allowed {
                    match plurx_core::transcode::manifest::load_with_budget(
                        directory.as_deref().expect("presence requires directory"),
                        scrub_remaining,
                    )
                    .await
                    {
                        Ok(Some((manifest, charged_bytes))) => {
                            debug_assert!(charged_bytes <= scrub_remaining);
                            scrub_remaining -= charged_bytes;
                            Some(manifest)
                        }
                        // The remaining deep-read budget is too small. The
                        // descriptor-bound presence heartbeat still rotates
                        // this holder; a later pass resumes its durable cursor.
                        Ok(None) => None,
                        Err(_) => {
                            // A malformed manifest is corruption, not merely a
                            // missed deep-scrub opportunity.
                            let _ = store
                                .invalidate_cache_entry(
                                    &entry.recipe_hash,
                                    node_id,
                                    &entry.storage_class,
                                    &entry.relative_dir,
                                    Some(expected_digest),
                                )
                                .await;
                            out.corrupt += 1;
                            continue;
                        }
                    }
                } else {
                    None
                };
                let mut valid = manifest_present
                    && manifest
                        .as_ref()
                        .is_none_or(|manifest| manifest.manifest_digest == expected_digest);
                let mut checked = 0usize;
                let mut next_object_index = entry.scrub_object_index.max(0) as usize;
                if let (Some(directory), Some(manifest)) = (directory.as_deref(), manifest.as_ref())
                {
                    if next_object_index >= manifest.objects.len() {
                        next_object_index = 0;
                    }
                    for object in manifest
                        .objects
                        .iter()
                        .cycle()
                        .skip(next_object_index)
                        .take(manifest.objects.len())
                    {
                        if readers.has_readers_besides(&entry.recipe_hash) {
                            break;
                        }
                        if scrubbed_objects >= MANIFEST_SCRUB_MAX_OBJECTS
                            || scrub_started.elapsed() >= MANIFEST_SCRUB_MAX_WALL
                        {
                            break;
                        }
                        let path = directory.join(&object.name);
                        let metadata = match tokio::fs::symlink_metadata(&path).await {
                            Ok(metadata)
                                if metadata.file_type().is_file()
                                    && !metadata.file_type().is_symlink()
                                    && metadata.len() == object.bytes =>
                            {
                                metadata
                            }
                            _ => {
                                valid = false;
                                break;
                            }
                        };
                        // Reserve one extra byte: the verifier performs one
                        // EOF read to prove a same-prefix larger file is not
                        // accepted, so even a concurrent replacement stays
                        // within this physical I/O ceiling.
                        let reserved = metadata.len().saturating_add(1);
                        if !reserve_scrub_bytes(&mut scrub_remaining, reserved) {
                            break;
                        }
                        match manifest.verify_object(directory, &object.name).await {
                            Ok(true) => {
                                checked += 1;
                                scrubbed_objects += 1;
                            }
                            Ok(false) | Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                }
                if valid {
                    let next = if checked > 0 {
                        let manifest = manifest.as_ref().expect("checked objects need manifest");
                        let next = (next_object_index + checked) % manifest.objects.len();
                        next as i64
                    } else {
                        entry.scrub_object_index.max(0)
                    };
                    manifest_checks.push(CacheManifestCheck {
                        recipe_hash: entry.recipe_hash.clone(),
                        node_id: node_id.to_owned(),
                        storage_class: entry.storage_class.clone(),
                        relative_dir: entry.relative_dir.clone(),
                        manifest_digest: expected_digest.to_owned(),
                        next_object_index: next,
                        observed_at: now.saturating_add(i64::from(checked > 0)),
                    });
                    continue;
                }
                match store
                    .invalidate_cache_entry(
                        &entry.recipe_hash,
                        node_id,
                        &entry.storage_class,
                        &entry.relative_dir,
                        Some(expected_digest),
                    )
                    .await
                {
                    Ok(true) => {
                        out.corrupt += 1;
                        if directory.is_none() {
                            tracing::warn!(
                                recipe = %entry.recipe_hash,
                                dir = %entry.relative_dir,
                                "cache: invalidated an unsafe generation path without touching the filesystem"
                            );
                        }
                        // Bytes become an orphan and are removed only by
                        // the guarded, ownership-rechecking orphan pass.
                    }
                    Ok(false) => tracing::debug!(
                        recipe = %entry.recipe_hash,
                        "cache: corrupt manifest belonged to a superseded location"
                    ),
                    Err(error) => tracing::error!(
                        recipe = %entry.recipe_hash,
                        %error,
                        "cache: could not invalidate a corrupt generation"
                    ),
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "cache: could not list manifest scrub candidates")
        }
    }
    out.scrub_bytes = MANIFEST_SCRUB_BYTES - scrub_remaining;
    if let Err(error) = store.mark_cache_manifests_checked(&manifest_checks).await {
        tracing::warn!(
            checked = manifest_checks.len(),
            %error,
            "cache: could not durably advance the manifest scrub cursors"
        );
    }

    // 3. Budget.
    let budget = budget_bytes(store).await;
    let mut used = store.cache_bytes(node_id).await.unwrap_or(0);
    let ceiling = budget.unwrap_or(0);
    if used > ceiling {
        // Coldest first. Asking for them in one page rather than one at a time
        // because the alternative re-queries per eviction, and the answer only
        // changes in the direction we are already walking.
        match store.cache_by_age(node_id, 512).await {
            Ok(cold) => {
                for entry in cold {
                    if used <= ceiling {
                        break;
                    }
                    let bytes = entry.bytes.max(0);
                    let _eviction = match readers.begin_eviction(&entry.recipe_hash) {
                        Ok(guard) => guard,
                        Err(CacheBusy::Readers) => {
                            out.protected += 1;
                            continue;
                        }
                        Err(CacheBusy::Evicting) => {
                            // A peer sweep is already changing the inventory
                            // this pass used to compute `used`. Continuing down
                            // the same snapshot would make both passes satisfy
                            // the full deficit independently and over-evict.
                            // End this pass; the peer owns the deletion and the
                            // next scheduled sweep will reconcile any failure.
                            out.in_flight += 1;
                            break;
                        }
                    };
                    if forget(store, root, node_id, &entry).await {
                        out.evicted += 1;
                        out.bytes_freed += bytes;
                        used -= bytes;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cache: could not list entries by age"),
        }
        if used > ceiling && out.in_flight > 0 {
            tracing::debug!(
                used,
                ceiling,
                in_flight = out.in_flight,
                "cache: peer sweep owns an eviction; ending this budget pass"
            );
        } else if used > ceiling && out.protected > 0 {
            tracing::warn!(
                used,
                ceiling,
                protected = out.protected,
                "cache: active playback keeps cache over budget; retrying on the next sweep"
            );
        } else if used > ceiling {
            // Said out loud rather than left to be inferred from a disk that
            // keeps growing. One page of evictions was not enough, which on a
            // 512-entry page means something is wrong with the sizes, not with
            // the walk.
            tracing::warn!(
                used,
                ceiling,
                "cache: still over budget after a full eviction pass"
            );
        }
    }
    out.bytes_after = used;

    // 4. Directories nothing claims.
    let (orphans, protected, in_flight, ownership_rechecks) =
        sweep_orphan_dirs(store, root, node_id, readers).await;
    out.orphans = orphans;
    out.protected += protected;
    out.in_flight += in_flight;
    out.ownership_rechecks = ownership_rechecks;
    if out.stale + out.evicted + out.corrupt + out.protected + out.in_flight + out.orphans > 0
        || out.scrub_bytes > 0
    {
        tracing::info!(
            stale = out.stale,
            evicted = out.evicted,
            corrupt = out.corrupt,
            protected = out.protected,
            in_flight = out.in_flight,
            orphans = out.orphans,
            ownership_rechecks = out.ownership_rechecks,
            freed = out.bytes_freed,
            scrub_bytes = out.scrub_bytes,
            used = out.bytes_after,
            "cache: swept"
        );
    }
    out
}

/// Tests that do not model an active HTTP reader use an empty registry. The
/// production entry points call [`sweep_with_readers`] with the transcode
/// manager's shared registry, so there is no unprotected production sweep.
#[cfg(test)]
async fn sweep(store: &Arc<dyn Store>, root: &Path, node_id: &str, now: i64) -> Swept {
    sweep_with_readers(store, root, node_id, &ActiveCacheReaders::default(), now).await
}

/// Delete an entry's bytes, then its row. Returns whether the row went.
///
/// Directory first, always. If the process dies between the two the row is left
/// pointing at nothing — and that row is a *hit*, which hands a viewer a
/// playlist for a directory that is gone. The other order leaves an orphan
/// directory instead, which the sweep below collects and which nothing serves.
async fn forget(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
    entry: &CachedTranscode,
) -> bool {
    let Some(dir) = entry_dir(root, &entry.relative_dir) else {
        // A row whose path escapes the root cannot be trusted to name what to
        // delete. Drop the row and leave the bytes, wherever they are.
        tracing::warn!(
            recipe = %entry.recipe_hash, dir = %entry.relative_dir,
            "cache: refusing to delete a path outside the cache root; dropping the row only"
        );
        return store
            .forget_cache_entry(&entry.recipe_hash, node_id, &entry.storage_class)
            .await
            .is_ok();
    };
    let identity = match plurx_core::fs_secure::directory_identity_nofollow(&dir).await {
        Ok(identity) => Some(identity),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(
                recipe = %entry.recipe_hash, dir = %dir.display(), %error,
                "cache: could not bind entry directory identity; keeping the row"
            );
            return false;
        }
    };
    if let Some(identity) = identity {
        let Some(parent) = dir.parent() else {
            return false;
        };
        let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let candidate = OrphanCandidate {
            path: dir.clone(),
            parent: parent.to_owned(),
            name: name.to_owned(),
            identity,
            max_depth: 0,
            _eviction: None,
        };
        if !quarantine_and_remove(&candidate).await {
            // The row stays, so a restored directory remains reachable and a
            // preserved quarantine remains visible to the orphan walker.
            return false;
        }
    }
    match store
        .forget_cache_entry(&entry.recipe_hash, node_id, &entry.storage_class)
        .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(recipe = %entry.recipe_hash, error = %e, "cache: could not forget entry");
            false
        }
    }
}

/// Resolve a row's relative directory under the root, refusing anything that
/// escapes it.
///
/// The rows are ours, so this should never fire — which is the reason to check
/// rather than to trust. It keeps later capability-relative quarantine work
/// inside the cache root; the distance between "a corrupt row" and "the media
/// library" is one `..`.
pub(crate) fn entry_dir(root: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative);
    if relative.trim().is_empty()
        || !rel
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(rel))
}

pub(crate) async fn validated_entry_dir(root: &Path, relative: &str) -> Option<PathBuf> {
    let path = entry_dir(root, relative)?;
    validate_directory_chain(root, &path).await.then_some(path)
}

pub(crate) async fn validate_directory_chain(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let Ok(root_metadata) = tokio::fs::symlink_metadata(root).await else {
        return false;
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.file_type().is_dir() {
        return false;
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        current.push(name);
        let Ok(metadata) = tokio::fs::symlink_metadata(&current).await else {
            return false;
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return false;
        }
    }
    true
}

/// Delete cache directories no row claims.
///
/// The cache is two levels deep (`ab/abcdef…`), the first level being a fanout
/// so no single directory holds thousands of entries. Both levels are walked
/// because a leftover can be either: a half-published entry under a live
/// prefix, or a whole prefix left by a producer that died before its first
/// rename.
///
/// [`STAGING`] sits at the same depth as a prefix and is handled by its own
/// rule below: what keeps a staging directory alive is the *claim* on its
/// recipe, not a published location, because a producer part-way through a
/// two-hour film has the former and cannot have the latter.
async fn sweep_orphan_dirs(
    store: &Arc<dyn Store>,
    root: &Path,
    node_id: &str,
    readers: &ActiveCacheReaders,
) -> (usize, usize, usize, usize) {
    // The common empty-install case should not ask the store for ownership or
    // emit an uncertainty warning. A missing root cannot contain bytes.
    if tokio::fs::metadata(root).await.is_err() {
        return (0, 0, 0, 0);
    }
    // Everything a row names, as absolute paths. Both complete and claimed: a
    // producer's publish destination is claimed and must not be swept out from
    // under it.
    // Filesystem ownership is not eviction policy. The filtered LRU and stale
    // queries intentionally hide offline-owned rows; using either as this
    // keep-list turns that protection inside out and deletes the protected
    // directories as "orphans".
    let snapshot = match ownership_snapshot(store, root, node_id).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            // Ownership uncertainty must fail closed. Treating a store error
            // as an empty keep-list would recursively delete valid media.
            tracing::warn!(%error, "cache: could not enumerate filesystem owners; skipping orphan sweep");
            return (0, 0, 0, 0);
        }
    };
    let authorized = orphan_inventory_authorized(&snapshot);
    let known = snapshot.paths;
    let claimed_recipes = snapshot.claimed_recipes;
    let queue_staging = snapshot.queue_jobs;
    if !authorized {
        // An incomplete all-empty answer contains no positive fact binding
        // this node's durable identity to the filesystem tree. A complete
        // empty inventory, by contrast, authoritatively says every candidate
        // is an orphan and is allowed to proceed to exact rechecks.
        tracing::warn!(
            "cache: filesystem owner inventory is incomplete and empty; skipping orphan sweep"
        );
        return (0, 0, 0, 0);
    }
    if !snapshot.complete {
        tracing::warn!(
            limit = 10_000,
            "cache: broad ownership inventory exceeded its scan ceiling; using exact bounded rechecks for deletion candidates"
        );
    }
    let mut final_candidates = Vec::with_capacity(ORPHAN_DELETE_LIMIT * 3 / 4);
    let mut staging_candidates = Vec::with_capacity(ORPHAN_DELETE_LIMIT / 4);
    let mut protected = 0usize;
    let mut in_flight = 0usize;
    // One guard per canonical recipe stays alive through *all* ownership
    // recheck batches. Embedding it in the first candidate would release it
    // when that candidate happened to fall in an earlier 128-row batch.
    let mut held_evictions = HashMap::new();
    let mut walker = readers.orphan_walker.lock().await;
    if walker.root.as_deref() != Some(root) {
        *walker = CacheOrphanWalker {
            root: Some(root.to_owned()),
            ..CacheOrphanWalker::default()
        };
    }

    // Persist the open directory cursors across passes. A hostile cache tree
    // therefore cannot force every sweep to restart at its first 10,000
    // entries, while the independent staging cursor prevents one huge fanout
    // prefix from starving crash-leftover cleanup. A prefix whose entries are
    // deleted below is reopened once: Darwin's directory cursor can otherwise
    // skip siblings when APFS compacts the directory around the cursor.
    let final_scan_limit = ORPHAN_SCAN_LIMIT * 4 / 5;
    let final_delete_limit = ORPHAN_DELETE_LIMIT * 3 / 4;
    let mut scanned = 0usize;
    let mut final_cursor_is_resume = false;
    while scanned < final_scan_limit && final_candidates.len() < final_delete_limit {
        if walker.final_entries.is_none() {
            if let Some(parent) = walker.final_resume.take() {
                if let Ok(entries) = tokio::fs::read_dir(&parent).await {
                    walker.final_entries = Some((parent, entries));
                    final_cursor_is_resume = true;
                    continue;
                }
            }
            if walker.prefixes.is_none() {
                walker.prefixes = tokio::fs::read_dir(root).await.ok();
            }
            let Some(prefixes) = walker.prefixes.as_mut() else {
                break;
            };
            let prefix = match prefixes.next_entry().await {
                Ok(Some(prefix)) => prefix,
                Ok(None) => {
                    walker.prefixes = None;
                    break;
                }
                Err(_) => {
                    walker.prefixes = None;
                    break;
                }
            };
            scanned += 1;
            if prefix.file_name() == STAGING
                || !prefix
                    .file_type()
                    .await
                    .map(|kind| kind.is_dir())
                    .unwrap_or(false)
            {
                continue;
            }
            let parent = prefix.path();
            if let Ok(entries) = tokio::fs::read_dir(&parent).await {
                walker.final_entries = Some((parent, entries));
                final_cursor_is_resume = false;
            }
            continue;
        }

        let (parent, next) = {
            let (parent, entries) = walker.final_entries.as_mut().expect("checked above");
            (parent.clone(), entries.next_entry().await)
        };
        let entry = match next {
            Ok(Some(entry)) => entry,
            Ok(None) | Err(_) => {
                walker.final_entries = None;
                continue;
            }
        };
        scanned += 1;
        let path = entry.path();
        if known.contains(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let identity = match plurx_core::fs_secure::directory_identity_nofollow(&path).await {
            Ok(identity) => identity,
            Err(_) => continue,
        };
        let eviction_key = final_recipe(&name).unwrap_or(&name).to_owned();
        if !held_evictions.contains_key(&eviction_key) {
            match readers.begin_eviction(&eviction_key) {
                Ok(guard) => {
                    held_evictions.insert(eviction_key.clone(), guard);
                }
                Err(CacheBusy::Readers) => {
                    protected += 1;
                    continue;
                }
                Err(CacheBusy::Evicting) => {
                    in_flight += 1;
                    continue;
                }
            };
        }
        final_candidates.push(OrphanCandidate {
            path,
            parent,
            name,
            identity,
            max_depth: 0,
            _eviction: None,
        });
    }

    let staging = root.join(STAGING);
    let mut staging_scanned = 0usize;
    while staging_scanned < ORPHAN_SCAN_LIMIT - final_scan_limit
        && staging_candidates.len() < ORPHAN_DELETE_LIMIT - final_delete_limit
    {
        if walker.staging_entries.is_none() {
            walker.staging_entries = tokio::fs::read_dir(&staging).await.ok();
        }
        let Some(entries) = walker.staging_entries.as_mut() else {
            break;
        };
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) | Err(_) => {
                walker.staging_entries = None;
                break;
            }
        };
        staging_scanned += 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        if staging_queue_job(&name).is_some_and(|job| queue_staging.contains(job))
            || claimed_recipes.contains(staging_recipe(&name))
        {
            continue;
        }
        let path = entry.path();
        let identity = match plurx_core::fs_secure::directory_identity_nofollow(&path).await {
            Ok(identity) => identity,
            Err(_) => continue,
        };
        let eviction_key = staging_recipe(&name).to_owned();
        if !held_evictions.contains_key(&eviction_key) {
            match readers.begin_eviction(&eviction_key) {
                Ok(guard) => {
                    held_evictions.insert(eviction_key.clone(), guard);
                }
                Err(CacheBusy::Readers) => {
                    protected += 1;
                    continue;
                }
                Err(CacheBusy::Evicting) => {
                    in_flight += 1;
                    continue;
                }
            };
        }
        staging_candidates.push(OrphanCandidate {
            path,
            parent: staging.clone(),
            name,
            identity,
            max_depth: 3,
            _eviction: None,
        });
    }

    // Never mutate a directory beneath a live readdir cursor. APFS may compact
    // the directory and advance the cursor past entries that have not yet been
    // returned. Resume an ordinary prefix once; after that bounded retry, move
    // on through the saved prefix cursor so a replenished prefix cannot starve
    // the rest of the cache tree.
    if let Some((parent, _)) = walker.final_entries.as_ref() {
        if final_candidates
            .iter()
            .any(|candidate| candidate.parent == *parent)
        {
            let parent = parent.clone();
            walker.final_entries = None;
            if !final_cursor_is_resume {
                walker.final_resume = Some(parent);
            }
        }
    }
    if !staging_candidates.is_empty() {
        walker.staging_entries = None;
    }
    drop(walker);

    let mut removed = 0usize;
    let mut ownership_rechecks = 0usize;
    while !final_candidates.is_empty() {
        let take = final_candidates.len().min(ORPHAN_RECHECK_BATCH);
        let mut batch = final_candidates.drain(..take).collect::<Vec<_>>();
        let (batch_removed, _) =
            delete_final_batch(store, root, node_id, readers, &mut batch).await;
        removed += batch_removed;
        ownership_rechecks += 1;
    }
    while !staging_candidates.is_empty() {
        let take = staging_candidates.len().min(ORPHAN_RECHECK_BATCH);
        let mut batch = staging_candidates.drain(..take).collect::<Vec<_>>();
        let (batch_removed, _) =
            delete_staging_batch(store, root, node_id, readers, &mut batch).await;
        removed += batch_removed;
        ownership_rechecks += 1;
    }
    drop(held_evictions);
    (removed, protected, in_flight, ownership_rechecks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::{open_store, StoreHandle};
    use plurx_core::config::Config;
    use plurx_core::domain::{
        ItemKind, LibraryKind, NewItem, NewLibrary, NewOfflinePackage, NewPretranscodeJob,
        OfflineCreateOutcome, PretranscodeRequirements, PretranscodeWorkerCapabilities,
        ProbeResult,
    };
    use plurx_core::store::{
        LibraryStore, MediaStore, OfflinePackageStore, SettingsStore, SqliteStore,
        TranscodeCacheStore, UserStore,
    };

    /// The real clock. These tests are about elapsed time, so `sweep` takes
    /// `now` as an argument; this is only the starting point they measure from.
    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn lease_now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0)
    }

    #[test]
    fn manifest_scrub_reservations_never_exceed_the_physical_budget() {
        let mut remaining = 10;
        assert!(reserve_scrub_bytes(&mut remaining, 4));
        assert_eq!(remaining, 6);
        assert!(!reserve_scrub_bytes(&mut remaining, 7));
        assert_eq!(remaining, 6, "a refused object consumed budget");
        assert!(reserve_scrub_bytes(&mut remaining, 6));
        assert_eq!(remaining, 0);
        assert!(!reserve_scrub_bytes(&mut remaining, 1));
        assert_eq!(
            MANIFEST_SCRUB_BYTES,
            plurx_core::transcode::manifest::MAX_OBJECT_BYTES
                + plurx_core::transcode::manifest::MAX_MANIFEST_BYTES
                + 2
        );
        let mut maximum = MANIFEST_SCRUB_BYTES;
        assert!(reserve_scrub_bytes(
            &mut maximum,
            plurx_core::transcode::manifest::MAX_MANIFEST_BYTES + 1
        ));
        assert!(reserve_scrub_bytes(
            &mut maximum,
            plurx_core::transcode::manifest::MAX_OBJECT_BYTES + 1
        ));
        assert_eq!(maximum, 0, "the largest valid row must make progress");
    }

    #[test]
    fn fenced_staging_names_retain_their_canonical_recipe_identity() {
        assert_eq!(staging_recipe("abcdef-f42"), "abcdef");
        assert_eq!(staging_recipe("abcdef-final"), "abcdef-final");
        assert_eq!(staging_recipe("abcdef-f"), "abcdef-f");
    }

    #[test]
    fn active_entry_snapshot_does_not_wait_for_the_ownership_map() {
        let readers = ActiveCacheReaders::default();
        let _reader = readers
            .begin_playback("recipe")
            .expect("playback reader claim");
        let states = readers.lock_states();
        let snapshot = readers.clone();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || sent.send(snapshot.active_entries()));
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_millis(100))
                .expect("atomic active-entry snapshot must not wait for the map"),
            1
        );
        drop(states);
        handle
            .join()
            .expect("join active-entry reader")
            .expect("send");
    }

    #[test]
    fn transient_cache_guards_leave_no_historical_ownership_entries() {
        let readers = ActiveCacheReaders::default();
        let lookup = readers.begin_lookup("miss").expect("lookup guard");
        let staging = (0..32)
            .map(|index| {
                readers
                    .begin_staging(&format!("recipe-{index:02}-f42"))
                    .expect("staging guard")
            })
            .collect::<Vec<_>>();
        let publication = readers
            .begin_publication("recipe", "recipe-f42")
            .expect("publication guard");
        assert_eq!(
            readers.active_entries(),
            0,
            "saturated staging/publication ownership is not active playback"
        );
        let internal_reads = (0..32)
            .map(|index| {
                readers
                    .begin_read(&format!("internal-{index:02}"))
                    .expect("internal reader")
            })
            .collect::<Vec<_>>();
        assert_eq!(readers.active_entries(), 0);
        let playback = readers.begin_playback("recipe").expect("playback guard");
        assert_eq!(readers.active_entries(), 1);
        drop(playback);
        assert_eq!(readers.active_entries(), 0);
        drop(publication);
        drop(internal_reads);
        drop(staging);
        drop(lookup);
        assert!(readers.lock_states().is_empty());
        assert_eq!(readers.active_entries(), 0);
    }

    #[test]
    fn distributed_staging_names_retain_their_queue_job_identity() {
        let job = "00000000-0000-4000-8000-000000000101";
        let recipe = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        assert_eq!(staging_queue_job(&format!("{recipe}-j{job}")), Some(job));
        assert_eq!(final_queue_job(&format!("{recipe}-j{job}-f42")), Some(job));
        assert_eq!(staging_queue_job(&format!("abcdef-j{job}")), None);
        assert_eq!(staging_queue_job("abcdef-jnot-a-uuid"), None);
        assert_eq!(staging_queue_job("abcdef"), None);
    }

    const NODE: &str = "node-a";

    async fn store() -> (Arc<dyn Store>, i64) {
        let store = SqliteStore::open_in_memory().expect("store");
        let lib = store
            .create_library(&NewLibrary {
                name: "M".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("lib");
        let movie = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Heat".into(),
                year: Some(1995),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let file = store
            .upsert_file(movie, "/m/Heat.mkv", 1, 1, &ProbeResult::default())
            .await
            .expect("file");
        (Arc::new(store) as Arc<dyn Store>, file)
    }

    /// A claimed directory with bytes in it, on disk and in the store.
    async fn claim(store: &Arc<dyn Store>, root: &Path, file: i64, hash: &str) -> PathBuf {
        let rel = format!("{}/{hash}", &hash[..2]);
        let dir = root.join(&rel);
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::write(dir.join("index.m3u8"), vec![b'x'; 16])
            .await
            .expect("playlist");
        store
            .claim_cache_entry(hash, file, 1, NODE, &rel)
            .await
            .expect("claim");
        dir
    }

    /// …and finished, so it can be served and evicted.
    async fn entry(
        store: &Arc<dyn Store>,
        root: &Path,
        file: i64,
        hash: &str,
        bytes: i64,
    ) -> PathBuf {
        let dir = claim(store, root, file, hash).await;
        store
            .complete_cache_entry(hash, NODE, bytes)
            .await
            .expect("complete");
        dir
    }

    async fn complete_fenced_location(
        store: &Arc<dyn Store>,
        file: i64,
        job_id: &str,
        recipe: &str,
        relative: &str,
        bytes: i64,
        manifest_digest: &str,
    ) {
        let lease_now = lease_now_ms();
        let lease = match store
            .acquire_lease(
                &format!("cachekeep-scrub-{job_id}"),
                "scheduler",
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("candidate lease")
        {
            plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) => lease,
            other => panic!("candidate lease held: {other:?}"),
        };
        let requirements = serde_json::to_string(&PretranscodeRequirements {
            version: PretranscodeRequirements::VERSION,
            decoder: "h264".to_owned(),
            acceptable_encoder_families: vec!["software".to_owned()],
            output_contract: "hls-mpegts-v1".to_owned(),
            tone_map: false,
            output_grade: "sdr".to_owned(),
            scratch_bytes: 1,
        })
        .expect("requirements");
        assert!(store
            .enqueue_pretranscode_job(
                &NewPretranscodeJob {
                    id: job_id.to_owned(),
                    dedupe_key: format!("cachekeep-scrub-{job_id}"),
                    file_id: file,
                    source_size: 1,
                    source_mtime: 1,
                    target_height: 720,
                    policy_generation: "scrub-v1".to_owned(),
                    requirements_json: requirements,
                    reason: "recent".to_owned(),
                    priority: 100,
                    not_before_ms: lease_now,
                    created_at_ms: lease_now,
                },
                &lease,
                &lease
                    .publication_successor()
                    .expect("publication successor"),
            )
            .await
            .expect("enqueue scrub fixture"));
        let capabilities = PretranscodeWorkerCapabilities {
            version: PretranscodeRequirements::VERSION,
            decoders: vec!["h264".to_owned()],
            encoder_families: vec!["software".to_owned()],
            max_target_height: 2_160,
            output_contracts: vec!["hls-mpegts-v1".to_owned()],
            tone_map: false,
            output_grades: vec!["sdr".to_owned()],
            scratch_bytes: 2,
        };
        let claimed = store
            .claim_pretranscode_job(
                NODE,
                &capabilities,
                &[],
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("claim scrub fixture")
            .expect("scrub fixture job");
        assert_eq!(claimed.id, job_id);
        assert!(store
            .complete_pretranscode_job(
                &claimed,
                recipe,
                1,
                relative,
                bytes,
                None,
                manifest_digest,
                lease_now.saturating_add(1),
            )
            .await
            .expect("complete scrub fixture"));
    }

    fn root() -> tempfile::TempDir {
        crate::test_tempdir().expect("root")
    }

    async fn preparing_package(store: &Arc<dyn Store>, file: i64, recipe: &str) -> (String, i64) {
        let user = store
            .create_user(&format!("user-{recipe}"), "hash", false)
            .await
            .expect("user");
        let package = NewOfflinePackage {
            id: format!("package-{recipe}"),
            request_id: format!("request-{recipe}"),
            user_id: user.id,
            file_id: file,
            node_id: NODE.into(),
            source_path: "/m/Heat.mkv".into(),
            source_size: 1,
            source_mtime: 1,
            effective_rate_control: "vbr".into(),
            target_height: 720,
            output_width: Some(1280),
            output_height: Some(720),
            audio_index: None,
            audio_offset_ms: 0,
            subtitle_index: None,
            subtitle_language: None,
            subtitle_mode: "none".into(),
            estimated_bytes: 10,
            reserved_bytes: 20,
            expires_at: i64::MAX,
        };
        assert!(matches!(
            store
                .create_offline_package(&package, 10, 1_000, 2_000)
                .await
                .expect("package"),
            OfflineCreateOutcome::Created(_)
        ));
        let claimed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim package")
            .expect("queued package");
        assert_eq!(claimed.id, package.id);
        assert!(store
            .set_offline_package_recipe(&package.id, recipe)
            .await
            .expect("bind recipe"));
        (package.id, user.id)
    }

    /// The budget is a ceiling and eviction stops the moment it is met — the
    /// cache is not supposed to empty itself over one entry too many. What goes
    /// is decided coldest-first by the store (tested there); what this owns is
    /// stopping at the right point, and taking the *bytes* rather than just the
    /// row. A budget that deletes rows and leaves directories is not a budget.
    #[tokio::test]
    async fn eviction_stops_as_soon_as_it_fits_and_takes_the_bytes_with_it() {
        let (store, file) = store().await;
        let root = root();
        store
            .put_setting(keys::CACHE_MAX_GB, "1")
            .await
            .expect("budget");
        // Three entries at 0.4 GB: 1.2 GB against a 1 GB ceiling. One has to go
        // and only one — 0.8 GB fits.
        let size = (0.4 * GB as f64) as i64;
        let mut dirs = Vec::new();
        for hash in ["aafirst", "bbsecond", "ccthird"] {
            dirs.push(entry(&store, root.path(), file, hash, size).await);
        }
        assert_eq!(store.cache_bytes(NODE).await.expect("bytes"), size * 3);

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.evicted, 1, "one is enough, so one is all that goes");
        assert_eq!(out.bytes_freed, size);
        assert!(out.bytes_after <= GB, "still over: {}", out.bytes_after);

        let gone: Vec<bool> = dirs.iter().map(|d| !d.exists()).collect();
        assert_eq!(
            gone.iter().filter(|g| **g).count(),
            1,
            "exactly one directory should have gone, got {gone:?}"
        );
        // Whichever it was, its row went with its bytes and vice versa.
        for (hash, dir) in ["aafirst", "bbsecond", "ccthird"].iter().zip(&dirs) {
            let row = store.cache_hit(hash, NODE).await.expect("hit").is_some();
            assert_eq!(
                row,
                dir.exists(),
                "{hash}: row and bytes disagree — one outlived the other"
            );
        }
    }

    /// `max_gb = 0` is how somebody turns the cache off, and turning it off has
    /// to reclaim the disk — that is the whole reason they touched the setting.
    #[tokio::test]
    async fn a_zero_budget_empties_the_cache() {
        let (store, file) = store().await;
        let root = root();
        let dir = entry(&store, root.path(), file, "aakeep", 100).await;
        assert!(
            budget_bytes(&store).await.is_some(),
            "the default is a budget"
        );

        store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("off");
        assert!(budget_bytes(&store).await.is_none(), "zero means off");
        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.evicted, 1);
        assert_eq!(out.bytes_after, 0);
        assert!(!dir.exists());
    }

    /// A corrupt row may lexically name a cache child while an intermediate
    /// component has been replaced by a symlink. Every eviction operation
    /// resolves the chain with component-wise `O_NOFOLLOW`; it must keep the
    /// row and leave the symlink target completely untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn eviction_rejects_an_intermediate_symlink_without_touching_its_target() {
        let (store, file) = store().await;
        let root = root();
        let outside = crate::test_tempdir().expect("outside");
        let victim = outside.path().join("victim");
        tokio::fs::create_dir_all(&victim).await.expect("victim");
        tokio::fs::write(victim.join("index.m3u8"), b"outside sentinel")
            .await
            .expect("sentinel");
        std::os::unix::fs::symlink(outside.path(), root.path().join("aa"))
            .expect("intermediate symlink");

        let recipe = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(store
            .claim_cache_entry(recipe, file, 1, NODE, "aa/victim")
            .await
            .expect("corrupt cache row"));
        store
            .complete_cache_entry(recipe, NODE, 16)
            .await
            .expect("complete corrupt row");
        store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("zero budget");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.evicted, 0);
        assert!(victim.join("index.m3u8").exists());
        assert!(store
            .cache_hit(recipe, NODE)
            .await
            .expect("retained unsafe row")
            .is_some());
    }

    /// A claim is either a producer at work or a producer that died, and only
    /// elapsed time tells them apart. Nothing about the claim below changes
    /// between the two sweeps — only the clock does, which is exactly the
    /// distinction being drawn.
    #[tokio::test]
    async fn a_claim_becomes_a_crash_leftover_only_with_age() {
        let (store, file) = store().await;
        let root = root();
        let now = unix_now();
        let dir = claim(&store, root.path(), file, "aaworking").await;

        let out = sweep(&store, root.path(), NODE, now).await;
        assert_eq!(out.stale, 0, "a fresh claim is a producer at work");
        assert!(
            dir.exists(),
            "a producer's output was deleted underneath it"
        );

        let out = sweep(&store, root.path(), NODE, now + STALE_CLAIM_SECS + 60).await;
        assert_eq!(out.stale, 1, "a day-old claim is a producer that died");
        assert!(!dir.exists());
    }

    /// A live producer's destination is claimed but not complete, and the
    /// orphan sweep must not read that as a leftover: it would delete the
    /// directory being written into, and the failure would surface much later
    /// as a corrupt entry, somewhere else entirely.
    #[tokio::test]
    async fn a_producer_at_work_is_not_an_orphan() {
        let (store, file) = store().await;
        let root = root();
        let dir = claim(&store, root.path(), file, "cdworking").await;
        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!((out.orphans, out.stale, out.evicted), (0, 0, 0));
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn a_published_offline_package_is_neither_evicted_nor_orphaned() {
        let (store, file) = store().await;
        let root = root();
        let (package_id, _user_id) = preparing_package(&store, file, "efready").await;
        let dir = entry(&store, root.path(), file, "efready", 100).await;
        assert!(store
            .mark_offline_package_ready(&package_id, NODE, "efready", 100, 90_000)
            .await
            .expect("ready"));
        store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("zero budget");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!((out.evicted, out.orphans), (0, 0));
        assert!(dir.join("index.m3u8").exists());
        assert!(store
            .cache_hit("efready", NODE)
            .await
            .expect("hit")
            .is_some());
    }

    /// Bytes with no row are the one leak a budget cannot see: every query says
    /// there is room and the filesystem disagrees. They arrive the ordinary
    /// way — a kill between writing the directory and committing the row.
    #[tokio::test]
    async fn directories_nothing_claims_are_reclaimed() {
        let (store, file) = store().await;
        let root = root();
        let kept = entry(&store, root.path(), file, "aakeep", 10).await;
        // A leftover beside a live entry, and a whole prefix of leftovers.
        tokio::fs::create_dir_all(root.path().join("aa/aaorphan"))
            .await
            .expect("mkdir");
        tokio::fs::write(root.path().join("aa/aaorphan/seg00000.ts"), b"junk")
            .await
            .expect("write");
        tokio::fs::create_dir_all(root.path().join("zz/zzorphan"))
            .await
            .expect("mkdir");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.orphans, 2);
        assert!(kept.join("index.m3u8").exists(), "a live entry was swept");
        assert!(!root.path().join("aa/aaorphan").exists());
        assert!(
            !root.path().join("zz").exists(),
            "an emptied prefix goes with its last entry"
        );
    }

    /// A cache row can disappear while its bytes are still being served: scan
    /// reconciliation cascades a vanished source file through the recipe and
    /// location tables. The orphan pass must honor the same reader ownership
    /// as LRU eviction, then collect the bytes after the last viewer leaves.
    #[tokio::test]
    async fn an_active_reader_survives_row_loss_and_the_orphan_pass() {
        let (store, file) = store().await;
        let root = root();
        let dir = entry(&store, root.path(), file, "aawatching", 100).await;
        let readers = ActiveCacheReaders::default();
        let reader = readers.begin_playback("aawatching").expect("reader claim");
        store
            .forget_cache_entry("aawatching", NODE, "local")
            .await
            .expect("remove row out from under reader");

        let active = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!((active.orphans, active.protected), (0, 1));
        assert!(
            dir.join("index.m3u8").exists(),
            "the orphan pass deleted bytes held by an active session"
        );

        drop(reader);
        let idle = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!((idle.orphans, idle.protected), (1, 0));
        assert!(
            !dir.exists(),
            "unowned bytes survived after the reader left"
        );
    }

    #[test]
    fn every_reader_must_leave_before_eviction_can_claim_a_recipe() {
        let readers = ActiveCacheReaders::default();
        let first = readers.begin_playback("recipe").expect("first reader");
        let second = readers.begin_playback("recipe").expect("second reader");
        assert_eq!(readers.active_entries(), 1);

        drop(first);
        assert!(matches!(
            readers.begin_eviction("recipe"),
            Err(CacheBusy::Readers)
        ));
        assert_eq!(readers.active_entries(), 1);

        drop(second);
        assert_eq!(readers.active_entries(), 0);
        assert!(readers.begin_eviction("recipe").is_ok());
    }

    #[test]
    fn a_lookup_during_eviction_is_an_ordinary_cache_miss() {
        let readers = ActiveCacheReaders::default();
        let eviction = readers.begin_eviction("recipe").expect("eviction claim");
        assert!(
            readers.begin_read("recipe").is_none(),
            "a lookup entered while deletion owned the recipe"
        );
        drop(eviction);
        assert!(readers.begin_read("recipe").is_some());
    }

    /// The staging area is not a fanout prefix, and the difference is hours of
    /// somebody's GPU.
    ///
    /// A producer part-way through a two-hour film has a claim and no published
    /// location — so judging its half-built asset by the rule that governs
    /// published ones deletes it, mid-encode, from the housekeeping job running
    /// beside it. The producer then finds its own files gone and fails, and the
    /// log blames ffmpeg.
    #[tokio::test]
    async fn the_sweep_does_not_delete_what_a_producer_is_building() {
        let (store, file) = store().await;
        let root = root();
        // A producer at work: a claim on the final path, bytes in staging.
        let staging = staging_dir(root.path(), "aaworking");
        tokio::fs::create_dir_all(staging.join("part-000"))
            .await
            .expect("mkdir");
        tokio::fs::write(staging.join("part-000/seg00000.ts"), b"half a film")
            .await
            .expect("write");
        store
            .claim_cache_entry("aaworking", file, 1, NODE, "aa/aaworking")
            .await
            .expect("claim");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.orphans, 0, "the sweep took a producer's work");
        assert!(
            staging.join("part-000/seg00000.ts").exists(),
            "an encode in progress was deleted by the housekeeping job beside it"
        );
    }

    #[tokio::test]
    async fn offline_preparation_keeps_its_staging_directory() {
        let (store, file) = store().await;
        let root = root();
        let (_package_id, _user_id) = preparing_package(&store, file, "ghoffline").await;
        store
            .claim_cache_entry("ghoffline", file, 1, NODE, "gh/ghoffline")
            .await
            .expect("cache claim");
        let staging = staging_dir(root.path(), "ghoffline");
        tokio::fs::create_dir_all(staging.join("part-000"))
            .await
            .expect("mkdir");
        tokio::fs::write(staging.join("part-000/seg00000.ts"), b"half a film")
            .await
            .expect("write");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!((out.stale, out.orphans), (0, 0));
        assert!(staging.join("part-000/seg00000.ts").exists());
    }

    /// …but staging that nothing claims is a producer that died. Nothing will
    /// pick it up — a later pass resumes from the *claim* — so it is bytes
    /// costing disk for no possible benefit.
    #[tokio::test]
    async fn staging_with_no_claim_behind_it_is_reclaimed() {
        let (store, file) = store().await;
        let root = root();
        // A neighboring owner proves that cleanup stays candidate-specific;
        // the broad inventory itself is complete with or without this row.
        entry(&store, root.path(), file, "aakeep", 1).await;
        let abandoned = staging_dir(root.path(), "bbabandoned");
        tokio::fs::create_dir_all(&abandoned).await.expect("mkdir");
        tokio::fs::write(abandoned.join("seg00000.ts"), b"junk")
            .await
            .expect("write");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.orphans, 1);
        assert!(!abandoned.exists());
        assert!(
            !root.path().join(STAGING).exists(),
            "an emptied staging area goes with its last directory"
        );
    }

    /// Producers acquire a shared-parent guard before ensuring either `tmp`
    /// or a fanout prefix. This models the vulnerable instant before their
    /// first child exists: cleanup may observe an empty parent, but must not
    /// unlink it until the producer has installed or abandoned its child.
    #[tokio::test]
    async fn producer_guards_close_empty_shared_parent_removal_gaps() {
        let root = root();
        let readers = ActiveCacheReaders::default();
        let recipe = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let final_name = format!("{recipe}-f1");
        let prefix = root.path().join("aa");
        tokio::fs::create_dir_all(&prefix).await.expect("fanout");

        let publication = readers
            .begin_publication(recipe, &final_name)
            .expect("publication guard");
        remove_empty_cache_parent(&readers, root.path(), &prefix).await;
        assert!(prefix.exists(), "cleanup unlinked a publisher's fanout");
        drop(publication);
        remove_empty_cache_parent(&readers, root.path(), &prefix).await;
        assert!(!prefix.exists(), "released empty fanout was retained");

        let staging_parent = root.path().join(STAGING);
        tokio::fs::create_dir_all(&staging_parent)
            .await
            .expect("staging parent");
        let staging = readers
            .begin_staging(recipe)
            .expect("staging producer guard");
        remove_empty_cache_parent(&readers, root.path(), &staging_parent).await;
        assert!(
            staging_parent.exists(),
            "cleanup unlinked a producer's staging parent"
        );
        drop(staging);
        remove_empty_cache_parent(&readers, root.path(), &staging_parent).await;
        assert!(
            !staging_parent.exists(),
            "released empty staging parent was retained"
        );
    }

    #[tokio::test]
    async fn queue_staging_survives_local_yield_and_is_reclaimed_after_remote_takeover() {
        let (store, file) = store().await;
        let root = root();
        let lease_now = lease_now_ms();
        let lease = match store
            .acquire_lease(
                "candidate:pretranscode",
                "scheduler",
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("candidate lease")
        {
            plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) => lease,
            other => panic!("candidate lease held: {other:?}"),
        };
        let requirements = serde_json::to_string(&PretranscodeRequirements {
            version: PretranscodeRequirements::VERSION,
            decoder: "h264".to_owned(),
            acceptable_encoder_families: vec!["software".to_owned()],
            output_contract: "hls-mpegts-v1".to_owned(),
            tone_map: false,
            output_grade: "sdr".to_owned(),
            scratch_bytes: 1,
        })
        .expect("requirements");
        let job_id = "00000000-0000-4000-8000-000000000201";
        assert!(store
            .enqueue_pretranscode_job(
                &NewPretranscodeJob {
                    id: job_id.to_owned(),
                    dedupe_key: "cachekeep-queue-job".to_owned(),
                    file_id: file,
                    source_size: 1,
                    source_mtime: 1,
                    target_height: 720,
                    policy_generation: "contract-v1".to_owned(),
                    requirements_json: requirements,
                    reason: "recent".to_owned(),
                    priority: 100,
                    not_before_ms: lease_now,
                    created_at_ms: lease_now,
                },
                &lease,
                &lease
                    .publication_successor()
                    .expect("publication successor"),
            )
            .await
            .expect("enqueue"));
        let capabilities = PretranscodeWorkerCapabilities {
            version: PretranscodeRequirements::VERSION,
            decoders: vec!["*".to_owned()],
            encoder_families: vec!["software".to_owned()],
            max_target_height: 2_160,
            output_contracts: vec!["hls-mpegts-v1".to_owned()],
            tone_map: true,
            output_grades: vec!["sdr".to_owned()],
            scratch_bytes: i64::MAX,
        };
        let first = store
            .claim_pretranscode_job(
                NODE,
                &capabilities,
                &[],
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("first claim")
            .expect("first job");
        let recipe = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let staging = root
            .path()
            .join(STAGING)
            .join(format!("{recipe}-j{job_id}"));
        tokio::fs::create_dir_all(staging.join("part-000"))
            .await
            .expect("staging");
        tokio::fs::write(staging.join("part-000/seg00000.ts"), b"checkpoint")
            .await
            .expect("checkpoint");
        let first_resume_at = lease_now_ms().saturating_add(1_000);
        assert!(store
            .yield_pretranscode_job(&first, first_resume_at, first_resume_at)
            .await
            .expect("yield"));
        sweep(&store, root.path(), NODE, unix_now()).await;
        assert!(staging.exists(), "a yielded local checkpoint was swept");

        let resumed = store
            .claim_pretranscode_job(
                NODE,
                &capabilities,
                &[],
                first_resume_at,
                first_resume_at.saturating_add(90_000),
            )
            .await
            .expect("local reclaim")
            .expect("local job");
        assert_eq!(resumed.id, job_id);
        let prefix = root.path().join("bb");
        tokio::fs::create_dir_all(&prefix).await.expect("fanout");
        let predecessor_final = prefix.join(format!("{recipe}-j{job_id}-f{}", first.fence));
        let current_final = prefix.join(format!("{recipe}-j{job_id}-f{}", resumed.fence));
        for final_dir in [&predecessor_final, &current_final] {
            tokio::fs::create_dir_all(final_dir)
                .await
                .expect("commit-unknown final");
            tokio::fs::write(final_dir.join("index.m3u8"), b"#EXTM3U\n#EXT-X-ENDLIST\n")
                .await
                .expect("playlist");
        }
        sweep(&store, root.path(), NODE, unix_now()).await;
        assert!(staging.exists(), "a locally reclaimed checkpoint was swept");
        assert!(
            !predecessor_final.exists() && current_final.exists(),
            "only the exact active fence may retain a commit-unknown final"
        );

        let second_resume_at = lease_now_ms().saturating_add(1_000);
        assert!(store
            .yield_pretranscode_job(&resumed, second_resume_at, second_resume_at)
            .await
            .expect("second yield"));
        let resumed_again = store
            .claim_pretranscode_job(
                NODE,
                &capabilities,
                &[],
                second_resume_at,
                second_resume_at.saturating_add(90_000),
            )
            .await
            .expect("second local reclaim")
            .expect("second local job");
        let next_final = prefix.join(format!("{recipe}-j{job_id}-f{}", resumed_again.fence));
        tokio::fs::create_dir_all(&next_final)
            .await
            .expect("next commit-unknown final");
        tokio::fs::write(next_final.join("index.m3u8"), b"#EXTM3U\n#EXT-X-ENDLIST\n")
            .await
            .expect("next playlist");
        sweep(&store, root.path(), NODE, unix_now()).await;
        assert!(
            !current_final.exists() && next_final.exists(),
            "a repeated retry must reclaim its obsolete fence generation"
        );

        let takeover_at = resumed_again.lease_expires_ms.saturating_add(1);
        let successor = store
            .claim_pretranscode_job(
                "node-b",
                &capabilities,
                &[],
                takeover_at,
                takeover_at.saturating_add(90_000),
            )
            .await
            .expect("remote takeover")
            .expect("remote job");
        assert_eq!(successor.fence, resumed_again.fence + 1);
        sweep(&store, root.path(), NODE, unix_now()).await;
        assert!(
            !staging.exists() && !next_final.exists(),
            "the predecessor's abandoned staging/final survived remote takeover"
        );
    }

    #[tokio::test]
    async fn manifest_scrub_yields_to_playback_and_resumes_durably_after_restart() {
        let (store, file) = store().await;
        let root = root();
        let recipe = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let relative = format!("aa/{recipe}");
        let directory = root.path().join(&relative);
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("generation");
        let mut names = vec!["index.m3u8".to_owned()];
        tokio::fs::write(directory.join("index.m3u8"), b"#EXTM3U\n#EXT-X-ENDLIST\n")
            .await
            .expect("playlist");
        for index in 0..16 {
            let name = format!("seg{index:05}.ts");
            tokio::fs::write(directory.join(&name), format!("segment-{index}"))
                .await
                .expect("segment");
            names.push(name);
        }
        let manifest =
            plurx_core::transcode::manifest::publish(&directory, "scrub-generation-1", &names)
                .await
                .expect("manifest");
        complete_fenced_location(
            &store,
            file,
            "00000000-0000-4000-8000-000000000401",
            recipe,
            &relative,
            1_000,
            &manifest.manifest_digest,
        )
        .await;
        let (package_id, user_id) = preparing_package(&store, file, recipe).await;
        assert!(store
            .mark_offline_package_ready(&package_id, NODE, recipe, 1_000, 90_000)
            .await
            .expect("ready scrubbed package"));

        let readers = ActiveCacheReaders::default();
        let playback = readers.begin_playback(recipe).expect("playback reader");
        let skipped = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!(skipped.scrub_bytes, 0, "scrub competed with playback");
        assert_eq!(
            store
                .cache_hit(recipe, NODE)
                .await
                .expect("cache hit")
                .expect("location")
                .scrub_object_index,
            0
        );
        drop(playback);

        let first = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert!(first.scrub_bytes <= MANIFEST_SCRUB_BYTES);
        assert_eq!(
            store
                .cache_hit(recipe, NODE)
                .await
                .expect("cache hit")
                .expect("location")
                .scrub_object_index,
            0,
        );

        // Requested-object verification is immediate; the next bounded
        // background pass also catches corruption introduced after a complete
        // small-generation rotation.
        tokio::fs::write(directory.join("seg00011.ts"), b"corrupt after restart")
            .await
            .expect("corrupt later object");
        let restarted_readers = ActiveCacheReaders::default();
        let second =
            sweep_with_readers(&store, root.path(), NODE, &restarted_readers, unix_now()).await;
        assert_eq!(second.corrupt, 1);
        assert!(second.scrub_bytes <= MANIFEST_SCRUB_BYTES);
        assert!(store
            .cache_hit(recipe, NODE)
            .await
            .expect("invalidated lookup")
            .is_none());
        let failed_package = store
            .offline_package_for_user(&package_id, user_id)
            .await
            .expect("scrubbed package lookup")
            .expect("scrubbed package");
        assert_eq!(failed_package.state, "failed");
        assert_eq!(failed_package.phase, "integrity");
        assert_eq!(
            failed_package.error_code.as_deref(),
            Some("cache_integrity")
        );
        assert!(!directory.exists(), "corrupt generation bytes survived");
    }

    #[tokio::test]
    async fn manifest_scrub_invalidates_unsafe_rows_without_leaving_the_cache_root() {
        let (store, file) = store().await;
        let sandbox = root();
        let cache_root = sandbox.path().join("cache");
        let outside = sandbox.path().join("outside");
        tokio::fs::create_dir_all(&cache_root)
            .await
            .expect("cache root");
        tokio::fs::create_dir_all(&outside).await.expect("outside");
        tokio::fs::write(outside.join("media.mkv"), b"must survive")
            .await
            .expect("outside sentinel");
        let recipes = [
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ];
        complete_fenced_location(
            &store,
            file,
            "00000000-0000-4000-8000-000000000402",
            recipes[0],
            "../outside",
            1,
            &"b".repeat(64),
        )
        .await;
        complete_fenced_location(
            &store,
            file,
            "00000000-0000-4000-8000-000000000403",
            recipes[1],
            &outside.to_string_lossy(),
            1,
            &"c".repeat(64),
        )
        .await;

        let swept = sweep_with_readers(
            &store,
            &cache_root,
            NODE,
            &ActiveCacheReaders::default(),
            unix_now(),
        )
        .await;
        assert_eq!(swept.corrupt, 2);
        assert!(outside.join("media.mkv").exists());
        for recipe in recipes {
            assert!(store
                .cache_hit(recipe, NODE)
                .await
                .expect("unsafe row lookup")
                .is_none());
        }
    }

    #[tokio::test]
    async fn queue_final_generation_is_protected_between_rename_and_fenced_completion() {
        let (store, _file) = store().await;
        let root = root();
        let readers = ActiveCacheReaders::default();
        let recipe = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let final_name = format!("{recipe}-j00000000-0000-4000-8000-000000000201-f1");
        let prefix = root.path().join("bb");
        tokio::fs::create_dir_all(&prefix).await.expect("prefix");
        let staging = root.path().join("publication-staging");
        tokio::fs::create_dir_all(&staging).await.expect("staging");
        tokio::fs::write(staging.join("index.m3u8"), b"#EXTM3U\n#EXT-X-ENDLIST\n")
            .await
            .expect("playlist");
        let final_dir = prefix.join(&final_name);
        let renamed = Arc::new(tokio::sync::Barrier::new(2));
        let completed = Arc::new(tokio::sync::Barrier::new(2));
        let publisher = {
            let readers = readers.clone();
            let renamed = Arc::clone(&renamed);
            let completed = Arc::clone(&completed);
            let staging = staging.clone();
            let final_dir = final_dir.clone();
            tokio::spawn(async move {
                let guard = readers
                    .begin_publication(recipe, &final_name)
                    .expect("publication ownership");
                tokio::fs::rename(&staging, &final_dir)
                    .await
                    .expect("publish rename");
                renamed.wait().await;
                completed.wait().await;
                drop(guard);
            })
        };

        renamed.wait().await;
        let during = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!(during.protected, 1);
        assert!(
            final_dir.exists(),
            "orphan sweep deleted a renamed generation before its fenced transaction"
        );

        completed.wait().await;
        publisher.await.expect("publisher task");
        let restarted_readers = ActiveCacheReaders::default();
        let after =
            sweep_with_readers(&store, root.path(), NODE, &restarted_readers, unix_now()).await;
        assert_eq!(after.orphans, 1);
        assert!(
            !final_dir.exists(),
            "unreferenced generation should become reclaimable after publication ownership ends"
        );
    }

    /// Budget eviction deletes bytes before forgetting its observed row. A
    /// publisher must not enter that gap: the late unversioned forget would
    /// otherwise erase the new location it just committed.
    #[tokio::test]
    async fn late_eviction_forget_and_queue_publication_are_mutually_exclusive() {
        let (store, file) = store().await;
        let root = root();
        let recipe = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        entry(&store, root.path(), file, recipe, 16).await;
        let old = store
            .cache_hit(recipe, NODE)
            .await
            .expect("old lookup")
            .expect("old location");
        let readers = ActiveCacheReaders::default();
        let bytes_deleted = Arc::new(tokio::sync::Barrier::new(2));
        let allow_forget = Arc::new(tokio::sync::Barrier::new(2));
        let evictor = {
            let store = Arc::clone(&store);
            let readers = readers.clone();
            let root = root.path().to_path_buf();
            let bytes_deleted = Arc::clone(&bytes_deleted);
            let allow_forget = Arc::clone(&allow_forget);
            tokio::spawn(async move {
                let _eviction = readers
                    .begin_eviction(recipe)
                    .expect("eviction owns recipe");
                tokio::fs::remove_dir_all(root.join(&old.relative_dir))
                    .await
                    .expect("delete old bytes");
                bytes_deleted.wait().await;
                allow_forget.wait().await;
                store
                    .forget_cache_entry(recipe, NODE, "local")
                    .await
                    .expect("forget old row");
            })
        };

        bytes_deleted.wait().await;
        let final_name = format!("{recipe}-j00000000-0000-4000-8000-000000000302-f1");
        assert!(
            readers.begin_publication(recipe, &final_name).is_none(),
            "queue publication entered after deletion but before the eviction's late forget"
        );
        allow_forget.wait().await;
        evictor.await.expect("evictor task");
        assert!(
            readers.begin_publication(recipe, &final_name).is_some(),
            "publication should retry after the eviction has fully settled"
        );
    }

    /// A sweep may read its inventory immediately before a queue publication
    /// commits. The final-directory eviction guard prevents a newer publisher,
    /// and this authoritative recheck protects the one that committed between
    /// the old snapshot and deletion.
    #[tokio::test]
    async fn stale_inventory_rechecks_after_queue_publication_before_delete() {
        let (store, file) = store().await;
        let root = root();
        let readers = ActiveCacheReaders::default();
        let recipe = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let final_name = format!("{recipe}-j00000000-0000-4000-8000-000000000304-f1");
        let relative = format!("ee/{final_name}");
        let final_dir = root.path().join(&relative);
        tokio::fs::create_dir_all(final_dir.parent().expect("fanout"))
            .await
            .expect("fanout");
        assert!(store
            .all_cache_rows(NODE)
            .await
            .expect("stale inventory snapshot")
            .is_empty());

        let committed = Arc::new(tokio::sync::Barrier::new(2));
        let publisher = {
            let store = Arc::clone(&store);
            let readers = readers.clone();
            let final_dir = final_dir.clone();
            let final_name = final_name.clone();
            let relative = relative.clone();
            let committed = Arc::clone(&committed);
            tokio::spawn(async move {
                let guard = readers
                    .begin_publication(recipe, &final_name)
                    .expect("publication guard");
                tokio::fs::create_dir_all(&final_dir)
                    .await
                    .expect("published directory");
                tokio::fs::write(final_dir.join("index.m3u8"), b"#EXTM3U\n#EXT-X-ENDLIST\n")
                    .await
                    .expect("playlist");
                assert!(store
                    .claim_cache_entry(recipe, file, 1, NODE, &relative)
                    .await
                    .expect("claim location"));
                store
                    .complete_cache_entry(recipe, NODE, 16)
                    .await
                    .expect("complete location");
                drop(guard);
                committed.wait().await;
            })
        };

        committed.wait().await;
        let eviction = readers
            .begin_eviction(&final_name)
            .expect("stale sweep owns candidate after publisher exits");
        let mut batch = vec![OrphanCandidate {
            path: final_dir.clone(),
            parent: final_dir.parent().expect("fanout").to_owned(),
            name: final_name,
            identity: plurx_core::fs_secure::directory_identity_nofollow(&final_dir)
                .await
                .expect("identity"),
            max_depth: 0,
            _eviction: Some(eviction),
        }];
        assert_eq!(
            delete_final_batch(&store, root.path(), NODE, &readers, &mut batch).await,
            (0, 1),
            "stale inventory would delete a generation published after its snapshot"
        );
        assert!(final_dir.exists());
        publisher.await.expect("publisher task");
    }

    #[tokio::test]
    async fn stale_staging_inventory_rechecks_a_new_claim_before_delete() {
        let (store, file) = store().await;
        let root = root();
        let recipe = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let staging = staging_dir(root.path(), recipe);
        tokio::fs::create_dir_all(&staging).await.expect("staging");
        assert!(store
            .all_cache_rows(NODE)
            .await
            .expect("stale inventory snapshot")
            .is_empty());
        let readers = ActiveCacheReaders::default();
        let eviction = readers
            .begin_eviction(recipe)
            .expect("stale sweep owns staging candidate");
        assert!(store
            .claim_cache_entry(recipe, file, 1, NODE, "ff/final")
            .await
            .expect("new producer claim"));
        let mut batch = vec![OrphanCandidate {
            path: staging.clone(),
            parent: staging.parent().expect("staging root").to_owned(),
            name: recipe.to_owned(),
            identity: plurx_core::fs_secure::directory_identity_nofollow(&staging)
                .await
                .expect("identity"),
            max_depth: 3,
            _eviction: Some(eviction),
        }];
        assert_eq!(
            delete_staging_batch(&store, root.path(), NODE, &readers, &mut batch).await,
            (0, 1),
            "stale staging inventory ignored a producer that claimed after its snapshot"
        );
        assert!(
            readers.begin_staging(recipe).is_some(),
            "staging ownership was not released after the safe recheck"
        );
        assert!(staging.exists());
    }

    #[tokio::test]
    async fn orphan_rechecks_are_bounded_by_candidate_pages() {
        let (store, _file) = store().await;
        let root = root();
        let recipe = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let prefix = root.path().join("aa");
        tokio::fs::create_dir_all(&prefix).await.expect("fanout");
        for fence in 1..=257 {
            let name = format!("{recipe}-j{}-f{fence}", uuid::Uuid::new_v4());
            tokio::fs::create_dir_all(prefix.join(name))
                .await
                .expect("queue orphan");
        }

        let readers = ActiveCacheReaders::default();
        let first = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!(first.orphans, ORPHAN_DELETE_LIMIT * 3 / 4);
        assert_eq!(
            first.ownership_rechecks, 2,
            "one pass must cap both deletion and ownership snapshots"
        );
        let second = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!(first.orphans + second.orphans, 257);
        assert_eq!(second.ownership_rechecks, 1);
    }

    /// A crashed producer is cleaned up in one pass, not two: the claim ages
    /// out, and the staging walk — which runs afterwards and re-reads the
    /// claims — no longer finds anything keeping its bytes.
    ///
    /// The ordering inside `sweep` is what makes that true, and it is the only
    /// reason these are separate steps rather than one.
    #[tokio::test]
    async fn a_crashed_producers_claim_and_bytes_go_together() {
        let (store, file) = store().await;
        let root = root();
        let now = unix_now();
        let staging = staging_dir(root.path(), "ccdead");
        tokio::fs::create_dir_all(&staging).await.expect("mkdir");
        tokio::fs::write(staging.join("seg00000.ts"), b"orphan")
            .await
            .expect("write");
        store
            .claim_cache_entry("ccdead", file, 1, NODE, "cc/ccdead")
            .await
            .expect("claim");

        let out = sweep(&store, root.path(), NODE, now + STALE_CLAIM_SECS + 60).await;
        assert_eq!((out.stale, out.orphans), (1, 1));
        assert!(
            store
                .stale_cache_claims(NODE, i64::MAX)
                .await
                .expect("claims")
                .is_empty(),
            "the stale claim survived"
        );
        assert!(
            !staging.exists(),
            "the bytes outlived every reference to them"
        );
    }

    /// The rows are ours, so a `..` in one should be impossible — which is
    /// exactly why it is checked rather than trusted. What it guards is a
    /// recursive delete, and the distance between a corrupt row and somebody's
    /// media library is one path component.
    #[test]
    fn a_row_cannot_name_a_directory_outside_the_cache() {
        let root = Path::new("/var/lib/plurx/cache/transcode");
        assert_eq!(
            entry_dir(root, "ab/abcdef"),
            Some(root.join("ab/abcdef")),
            "an ordinary entry still resolves"
        );
        for bad in ["../../../media", "ab/../../media", "/etc", "", "   "] {
            assert!(entry_dir(root, bad).is_none(), "{bad:?} should be refused");
        }
    }

    /// Bytes that will not delete keep their row. Dropping it would strand them
    /// permanently: nothing else records that they exist, so no later sweep
    /// could find them, and the budget would go on counting disk it can never
    /// reclaim.
    ///
    /// The failure is injected by making the entry's path a *file*, which
    /// `remove_dir_all` refuses for every user. Permissions would be the more
    /// natural injection and were the first attempt; they are useless here,
    /// because root ignores them and containers run as root.
    #[tokio::test]
    async fn bytes_that_will_not_delete_keep_their_row() {
        let (store, file) = store().await;
        let root = root();
        tokio::fs::create_dir_all(root.path().join("aa"))
            .await
            .expect("prefix");
        tokio::fs::write(root.path().join("aa/aastuck"), b"not a directory")
            .await
            .expect("write");
        store
            .claim_cache_entry("aastuck", file, 1, NODE, "aa/aastuck")
            .await
            .expect("claim");
        store
            .complete_cache_entry("aastuck", NODE, 100)
            .await
            .expect("complete");

        store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("off");
        let out = sweep(&store, root.path(), NODE, unix_now()).await;

        assert_eq!(out.evicted, 0, "nothing was actually freed");
        assert!(
            store
                .cache_hit("aastuck", NODE)
                .await
                .expect("hit")
                .is_some(),
            "the row went but the bytes did not — they are unreachable forever now"
        );
        assert!(
            root.path().join("aa/aastuck").exists(),
            "and the bytes are still there, so the next sweep gets another go"
        );
    }

    /// Two sweeps use the same cold snapshot. Once one owns an entry, the
    /// other must not walk farther and satisfy the same deficit against a
    /// disjoint set — doing so lets two correct-looking passes empty a cache.
    #[tokio::test]
    async fn a_peer_eviction_ends_the_budget_pass_before_it_over_evicts() {
        let (store, file) = store().await;
        let root = root();
        store
            .put_setting(keys::CACHE_MAX_GB, "1")
            .await
            .expect("budget");
        let size = (0.4 * GB as f64) as i64;
        for hash in ["aafirst", "bbsecond", "ccthird"] {
            entry(&store, root.path(), file, hash, size).await;
        }

        let peer_entry = store
            .cache_by_age(NODE, 10)
            .await
            .expect("cold inventory")
            .into_iter()
            .next()
            .expect("first cold entry");
        let readers = ActiveCacheReaders::default();
        let peer = readers
            .begin_eviction(&peer_entry.recipe_hash)
            .expect("peer sweep owns the first eviction");

        let out = sweep_with_readers(&store, root.path(), NODE, &readers, unix_now()).await;
        assert_eq!(out.evicted, 0, "the losing pass evicted a second entry");
        assert_eq!(out.in_flight, 1);
        assert_eq!(
            store.cache_bytes(NODE).await.expect("bytes before peer"),
            size * 3,
            "the losing pass changed the peer's inventory"
        );

        drop(peer);
        assert!(forget(&store, root.path(), NODE, &peer_entry).await);
        assert_eq!(
            store.cache_bytes(NODE).await.expect("bytes after peer"),
            size * 2,
            "one peer eviction should be exactly enough to meet the ceiling"
        );
    }

    /// The broad inventory contract marks a complete empty answer as
    /// authoritative. Exact candidate rechecks still run before deletion, so
    /// a node with no durable owners can reclaim otherwise invisible bytes.
    #[tokio::test]
    async fn a_complete_empty_ownership_inventory_authorizes_exact_cleanup() {
        let (store, _file) = store().await;
        let root = root();
        let orphan = root.path().join("aa/aaunverified");
        tokio::fs::create_dir_all(&orphan).await.expect("orphan");
        tokio::fs::write(orphan.join("seg00000.ts"), b"unverified bytes")
            .await
            .expect("bytes");

        let out = sweep(&store, root.path(), NODE, unix_now()).await;
        assert_eq!(out.orphans, 1);
        assert!(
            !orphan.exists(),
            "a complete empty inventory did not reclaim an exact orphan"
        );
    }

    #[test]
    fn an_incomplete_empty_ownership_inventory_fails_closed() {
        let incomplete_empty = OwnershipSnapshot {
            paths: HashSet::new(),
            claimed_recipes: HashSet::new(),
            queue_jobs: HashSet::new(),
            has_owners: false,
            complete: false,
        };
        assert!(!orphan_inventory_authorized(&incomplete_empty));

        let complete_empty = OwnershipSnapshot {
            complete: true,
            ..incomplete_empty
        };
        assert!(orphan_inventory_authorized(&complete_empty));

        let incomplete_with_positive_owner = OwnershipSnapshot {
            paths: HashSet::new(),
            claimed_recipes: HashSet::from(["working".to_owned()]),
            queue_jobs: HashSet::new(),
            has_owners: true,
            complete: false,
        };
        assert!(orphan_inventory_authorized(&incomplete_with_positive_owner));
    }

    /// The sweep runs against a server that has never produced anything far
    /// more often than against one that has. None of it may fail, log alarming
    /// things, or create the root as a side effect of looking at it.
    #[tokio::test]
    async fn an_empty_cache_sweeps_to_nothing() {
        let (store, _file) = store().await;
        let missing = root().path().join("never-made");
        assert_eq!(
            sweep(&store, &missing, NODE, unix_now()).await,
            Swept::default()
        );
        assert!(!missing.exists(), "looking is not creating");
    }

    /// M0's compatibility fixture. Before clustering, both cache locations and
    /// offline packages were owned by `instance.id`. Initializing `node.id`
    /// must seed that exact value before either cleanup path runs, or the cache
    /// directory becomes an orphan and the offline worker loses its queue.
    #[tokio::test]
    async fn node_identity_initialization_preserves_populated_v14_ownership_and_bytes() {
        let data = crate::test_tempdir().expect("data dir");
        let mut config = Config::default();
        config.storage.data_dir = data.path().to_owned();
        let cache_root = data.path().join("cache/transcode");
        let entry_dir = cache_root.join("aa/aakeep");
        let ready_id = "offline-ready";
        let interrupted_id = "offline-interrupted";
        let (cluster_id, user_id, file_id) = {
            // Populate the v14 store before any M0 identity code has run.
            let store = SqliteStore::open(&data.path().join("plurx.db")).expect("v14 store");
            let cluster_id = store.instance_id().await.expect("instance id");

            let library = store
                .create_library(&NewLibrary {
                    name: "Movies".into(),
                    kind: LibraryKind::Movies,
                    paths: vec![],
                    anime: false,
                })
                .await
                .expect("library");
            let movie = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: "Heat".into(),
                    year: Some(1995),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("movie");
            let file_id = store
                .upsert_file(movie, "/m/Heat.mkv", 1, 1, &ProbeResult::default())
                .await
                .expect("file");
            let user = store
                .create_user("traveller", "hash", false)
                .await
                .expect("user");

            tokio::fs::create_dir_all(&entry_dir)
                .await
                .expect("cache entry dir");
            tokio::fs::write(entry_dir.join("index.m3u8"), b"durable cached bytes")
                .await
                .expect("cache bytes");
            store
                .claim_cache_entry("aakeep", file_id, 1, &cluster_id, "aa/aakeep")
                .await
                .expect("cache claim");
            store
                .complete_cache_entry("aakeep", &cluster_id, 20)
                .await
                .expect("complete cache");

            let package = |id: &str, request: &str| NewOfflinePackage {
                id: id.into(),
                request_id: request.into(),
                user_id: user.id,
                file_id,
                node_id: cluster_id.clone(),
                source_path: "/m/Heat.mkv".into(),
                source_size: 1,
                source_mtime: 1,
                effective_rate_control: "vbr".into(),
                target_height: 720,
                output_width: Some(1280),
                output_height: Some(720),
                audio_index: None,
                audio_offset_ms: 0,
                subtitle_index: None,
                subtitle_language: None,
                subtitle_mode: "none".into(),
                estimated_bytes: 10,
                reserved_bytes: 20,
                expires_at: i64::MAX,
            };

            assert!(matches!(
                store
                    .create_offline_package(&package(ready_id, "request-ready"), 10, 1_000, 2_000)
                    .await
                    .expect("ready package"),
                OfflineCreateOutcome::Created(_)
            ));
            assert_eq!(
                store
                    .claim_next_offline_package(&cluster_id)
                    .await
                    .expect("claim ready")
                    .expect("ready package exists")
                    .id,
                ready_id
            );
            store
                .set_offline_package_recipe(ready_id, "aakeep")
                .await
                .expect("bind recipe");
            assert!(store
                .mark_offline_package_ready(ready_id, &cluster_id, "aakeep", 20, 90_000)
                .await
                .expect("publish ready package"));

            assert!(matches!(
                store
                    .create_offline_package(
                        &package(interrupted_id, "request-interrupted"),
                        10,
                        1_000,
                        2_000,
                    )
                    .await
                    .expect("interrupted package"),
                OfflineCreateOutcome::Created(_)
            ));
            assert_eq!(
                store
                    .claim_next_offline_package(&cluster_id)
                    .await
                    .expect("claim interrupted")
                    .expect("interrupted package exists")
                    .id,
                interrupted_id
            );
            (cluster_id, user.id, file_id)
        };

        assert!(
            !data.path().join("node.id").exists(),
            "the pre-M0 fixture must not initialize M0 identity"
        );

        // Upgrade through the real M0 path, then run both startup cleanup
        // behaviors against the newly initialized node-local id.
        let StoreHandle {
            store, identity, ..
        } = open_store(&config).await.expect("M0 open");
        assert_eq!(identity.cluster_id, cluster_id);
        assert_eq!(identity.node_id, cluster_id);
        assert_eq!(
            std::fs::read_to_string(data.path().join("node.id")).expect("node id file"),
            format!("{cluster_id}\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(data.path().join("node.id"))
                    .expect("node id metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        // Arm orphan collection with a row created for the current identity.
        // If the upgrade picked a different id, the pre-M0 entry is now a
        // visible orphan and this same sweep deletes it.
        let current_entry = cache_root.join("bb/bbcurrent");
        tokio::fs::create_dir_all(&current_entry)
            .await
            .expect("current cache entry dir");
        tokio::fs::write(current_entry.join("index.m3u8"), b"current cached bytes")
            .await
            .expect("current cache bytes");
        store
            .claim_cache_entry("bbcurrent", file_id, 1, &identity.node_id, "bb/bbcurrent")
            .await
            .expect("current cache claim");
        store
            .complete_cache_entry("bbcurrent", &identity.node_id, 20)
            .await
            .expect("complete current cache");
        assert_eq!(
            store
                .reset_interrupted_offline_packages(&identity.node_id)
                .await
                .expect("recover interrupted package"),
            1
        );

        let swept = sweep_with_readers(
            &store,
            &cache_root,
            &identity.node_id,
            &ActiveCacheReaders::default(),
            unix_now(),
        )
        .await;
        assert_eq!((swept.stale, swept.evicted, swept.orphans), (0, 0, 0));
        assert!(
            entry_dir.join("index.m3u8").exists(),
            "identity initialization let cache cleanup delete owned bytes"
        );
        assert!(store
            .cache_hit("aakeep", &identity.node_id)
            .await
            .expect("cache lookup")
            .is_some());
        assert_eq!(
            store
                .offline_package_for_user(ready_id, user_id)
                .await
                .expect("offline lookup")
                .expect("ready package retained")
                .state,
            "ready"
        );
        assert_eq!(
            store
                .claim_next_offline_package(&identity.node_id)
                .await
                .expect("reclaimed queue")
                .expect("interrupted package was requeued")
                .id,
            interrupted_id
        );
    }

    #[tokio::test]
    async fn a_distinct_node_id_cannot_strand_existing_local_ownership() {
        let data = crate::test_tempdir().expect("data dir");
        let mut config = Config::default();
        config.storage.data_dir = data.path().to_owned();
        let store = SqliteStore::open(&data.path().join("plurx.db")).expect("store");
        let cluster_id = store.instance_id().await.expect("instance id");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let movie = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Heat".into(),
                year: Some(1995),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let file_id = store
            .upsert_file(movie, "/m/Heat.mkv", 1, 1, &ProbeResult::default())
            .await
            .expect("file");
        store
            .claim_cache_entry("aakeep", file_id, 1, &cluster_id, "aa/aakeep")
            .await
            .expect("legacy cache claim");
        drop(store);

        let distinct = uuid::Uuid::new_v4().to_string();
        std::fs::write(data.path().join("node.id"), format!("{distinct}\n"))
            .expect("distinct node id");
        let error = match open_store(&config).await {
            Ok(_) => panic!("a distinct node id must not strand legacy ownership"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("refusing to strand owned bytes"));
    }
}
