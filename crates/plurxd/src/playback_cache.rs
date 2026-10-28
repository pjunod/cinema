//! One physical disk budget for every disposable playback namespace.
//!
//! Producers reserve before taking their component locks. Reservations charge
//! future writes and pin their namespace; readers pin for the whole response.
//! Eviction selects one global LRU, but each storage owner remains responsible
//! for its own durable dependencies, writer fencing and safe unlink protocol.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use plurx_core::store::{keys, Store};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;

const GIB: u64 = 1024 * 1024 * 1024;
const DEFAULT_BUDGET: u64 = 50 * GIB;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const PRESSURE_TIMEOUT: Duration = Duration::from_secs(30);
const SAMPLE_AGE: Duration = Duration::from_secs(1);
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub(crate) struct CacheCandidate {
    pub path: PathBuf,
    pub last_used: SystemTime,
}

#[async_trait::async_trait]
pub(crate) trait CacheOwner: Send + Sync {
    async fn candidates(&self) -> io::Result<Vec<CacheCandidate>>;
    async fn evict(&self, candidate: &CacheCandidate) -> io::Result<bool>;
}

#[derive(Debug, Default)]
pub(crate) struct SweepReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub evicted: usize,
    pub protected: usize,
}

#[derive(Clone, Copy, Debug)]
struct DiskSpace {
    free: u64,
    total: u64,
    allocation_unit: u64,
}

impl DiskSpace {
    fn floor(self) -> u64 {
        GIB.max(self.total / 50)
    }
}

#[derive(Debug)]
struct Sample {
    at: Instant,
    bytes: u64,
    disks: HashMap<u64, DiskSpace>,
    root_devices: Vec<u64>,
    committed: u128,
    device_committed: HashMap<u64, u128>,
}

#[derive(Debug)]
struct Pending {
    bytes: u64,
    device: u64,
    allocation_unit: u64,
}

#[derive(Default, Debug)]
struct State {
    next: u64,
    pins: HashMap<u64, PathBuf>,
    evicting: HashMap<u64, PathBuf>,
    pending: HashMap<u64, Pending>,
    committed: u128,
    device_committed: HashMap<u64, u128>,
    sample: Option<Sample>,
    dirty: bool,
    dirty_revision: u64,
    ceiling: u64,
    setting_at: Option<Instant>,
}

impl State {
    fn id(&mut self) -> u64 {
        self.next = self
            .next
            .checked_add(1)
            .expect("playback cache lease ids exhausted");
        self.next
    }

    fn used(&self) -> u64 {
        let Some(sample) = self.sample.as_ref() else {
            return u64::MAX;
        };
        sample
            .bytes
            .saturating_add(narrow(self.committed.saturating_sub(sample.committed)))
            .saturating_add(
                self.pending
                    .values()
                    .fold(0u64, |sum, pending| sum.saturating_add(pending.bytes)),
            )
    }

    fn disk_remaining(&self, device: u64) -> u64 {
        let Some(sample) = self.sample.as_ref() else {
            return 0;
        };
        let Some(space) = sample.disks.get(&device).copied() else {
            return 0;
        };
        let committed = self
            .device_committed
            .get(&device)
            .copied()
            .unwrap_or(0)
            .saturating_sub(sample.device_committed.get(&device).copied().unwrap_or(0));
        let pending = self
            .pending
            .values()
            .filter(|pending| pending.device == device)
            .fold(0u64, |sum, pending| sum.saturating_add(pending.bytes));
        space
            .free
            .saturating_sub(space.floor())
            .saturating_sub(narrow(committed))
            .saturating_sub(pending)
    }

    fn remaining(&self, device: u64) -> u64 {
        self.ceiling
            .saturating_sub(self.used())
            .min(self.disk_remaining(device))
    }

    fn pressure(&self, request: Option<(u64, u64)>) -> bool {
        if let Some((device, bytes)) = request {
            self.ceiling == 0 || self.remaining(device) < bytes
        } else {
            self.used() > self.ceiling
                || self.sample.as_ref().is_none_or(|sample| {
                    sample.disks.iter().any(|(device, space)| {
                        let committed = self
                            .device_committed
                            .get(device)
                            .copied()
                            .unwrap_or(0)
                            .saturating_sub(
                                sample.device_committed.get(device).copied().unwrap_or(0),
                            );
                        let pending = self
                            .pending
                            .values()
                            .filter(|pending| pending.device == *device)
                            .fold(0u64, |sum, pending| sum.saturating_add(pending.bytes));
                        space.free
                            < space
                                .floor()
                                .saturating_add(narrow(committed))
                                .saturating_add(pending)
                    })
                })
        }
    }
}

fn narrow(bytes: u128) -> u64 {
    bytes.min(u128::from(u64::MAX)) as u64
}
fn allocation_charge(bytes: u64, unit: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    let unit = u128::from(unit.max(1));
    narrow(u128::from(bytes).div_ceil(unit).saturating_mul(unit))
}

fn overlapping(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn normalized(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cache path escapes filesystem root",
                    ));
                }
            }
            component => out.push(component.as_os_str()),
        }
    }
    Ok(out)
}

fn registry() -> &'static Mutex<Vec<Weak<PlaybackCacheBudget>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<PlaybackCacheBudget>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Lookup is weak: test fixtures and replaced daemon owners do not live forever.
pub(crate) fn lookup(path: &Path) -> Option<Arc<PlaybackCacheBudget>> {
    let path = normalized(path).ok()?;
    let mut entries = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.retain(|entry| entry.strong_count() > 0);
    entries
        .iter()
        .filter_map(Weak::upgrade)
        .filter_map(|budget| {
            budget
                .root_index(&path)
                .map(|index| (budget.roots[index].components().count(), budget))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, budget)| budget)
}

/// An absent coordinator preserves isolated component-test behavior. Production
/// registers all namespaces before serving requests or starting producers.
pub(crate) async fn reserve(path: &Path, bytes: u64) -> io::Result<Option<CacheReservation>> {
    let Some(budget) = lookup(path) else {
        return Ok(None);
    };
    budget.reserve_owned(path, bytes).await.map(Some)
}

pub(crate) fn pin(path: &Path) -> io::Result<Option<CachePin>> {
    let Some(budget) = lookup(path) else {
        return Ok(None);
    };
    let path = normalized(path)?;
    let mut state = budget.lock();
    if state
        .evicting
        .values()
        .any(|candidate| overlapping(candidate, &path))
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "playback cache entry is being evicted",
        ));
    }
    let id = state.id();
    state.pins.insert(id, path);
    drop(state);
    Ok(Some(CachePin { budget, id }))
}

/// For an owner's independent orphan/stale cleanup. Coordinator callbacks
/// already hold this fence and must not acquire it a second time.
pub(crate) fn begin_eviction(path: &Path) -> io::Result<Option<CacheEvictionGuard>> {
    let Some(budget) = lookup(path) else {
        return Ok(None);
    };
    let path = normalized(path)?;
    let mut state = budget.lock();
    if state
        .pins
        .values()
        .chain(state.evicting.values())
        .any(|candidate| overlapping(candidate, &path))
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "playback cache namespace is busy",
        ));
    }
    let id = state.id();
    state.evicting.insert(id, path);
    drop(state);
    Ok(Some(CacheEvictionGuard {
        budget,
        id,
        settled: false,
    }))
}

/// Advisory projection for owner maintenance. Atomic deletion exclusion belongs
/// to the coordinator's eviction claim, rather than a check-then-unlink call.
pub(crate) fn is_pinned(path: &Path) -> bool {
    let Some(budget) = lookup(path) else {
        return false;
    };
    let Ok(path) = normalized(path) else {
        return true;
    };
    let pinned = budget
        .lock()
        .pins
        .values()
        .any(|candidate| overlapping(candidate, &path));
    pinned
}

pub(crate) struct PlaybackCacheBudget {
    store: Arc<dyn Store>,
    roots: Vec<PathBuf>,
    owners: Mutex<Vec<Arc<dyn CacheOwner>>>,
    state: Mutex<State>,
    operations: AsyncMutex<()>,
    // The permit moves into the blocking closure. Cancelling its waiter cannot
    // start another scan while a slow/hung filesystem still owns this one.
    io_slot: Arc<Semaphore>,
    callback_slot: Arc<Semaphore>,
    #[cfg(test)]
    test_ceiling: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    test_space: Mutex<Option<DiskSpace>>,
}

impl std::fmt::Debug for PlaybackCacheBudget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlaybackCacheBudget")
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}

impl PlaybackCacheBudget {
    pub(crate) fn new(store: Arc<dyn Store>, roots: Vec<PathBuf>) -> Arc<Self> {
        let mut roots = roots
            .into_iter()
            .map(|path| normalized(&path).expect("valid cache root"))
            .collect::<Vec<_>>();
        roots.sort_by_key(|path| path.components().count());
        let mut disjoint = Vec::<PathBuf>::new();
        for root in roots {
            if !disjoint.iter().any(|parent| root.starts_with(parent)) {
                disjoint.push(root);
            }
        }
        let budget = Arc::new(Self {
            store,
            roots: disjoint,
            owners: Mutex::default(),
            state: Mutex::default(),
            operations: AsyncMutex::new(()),
            io_slot: Arc::new(Semaphore::new(1)),
            callback_slot: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            test_ceiling: std::sync::atomic::AtomicU64::new(u64::MAX),
            #[cfg(test)]
            test_space: Mutex::new(None),
        });
        registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&budget));
        budget
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn root_index(&self, path: &Path) -> Option<usize> {
        self.roots.iter().position(|root| path.starts_with(root))
    }

    pub(crate) fn register(&self, owner: Arc<dyn CacheOwner>) {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(owner);
    }

    async fn operation(&self) -> io::Result<tokio::sync::MutexGuard<'_, ()>> {
        tokio::time::timeout(IO_TIMEOUT, self.operations.lock())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "playback cache maintenance is busy",
                )
            })
    }

    async fn refresh(&self, force: bool) -> io::Result<()> {
        let refresh_setting = {
            let state = self.lock();
            force || state.setting_at.is_none_or(|at| at.elapsed() >= SAMPLE_AGE)
        };
        if refresh_setting {
            let value =
                tokio::time::timeout(IO_TIMEOUT, self.store.get_setting(keys::CACHE_MAX_GB))
                    .await
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "playback cache budget setting timed out",
                        )
                    })?
                    .map_err(io::Error::other)?;
            let ceiling = match value {
                None => DEFAULT_BUDGET,
                Some(value) => value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "cache.max_gb is not a non-negative integer",
                        )
                    })?
                    .saturating_mul(GIB),
            };
            #[cfg(test)]
            let ceiling = ceiling.min(self.test_ceiling.load(std::sync::atomic::Ordering::Relaxed));
            let mut state = self.lock();
            state.ceiling = ceiling;
            state.setting_at = Some(Instant::now());
        }
        let (needed, committed, device_committed, dirty_revision) = {
            let state = self.lock();
            (
                force
                    || state.dirty
                    || state
                        .sample
                        .as_ref()
                        .is_none_or(|sample| sample.at.elapsed() >= SAMPLE_AGE),
                state.committed,
                state.device_committed.clone(),
                state.dirty_revision,
            )
        };
        if !needed {
            return Ok(());
        }
        self.lock().dirty = true;
        let permit = tokio::time::timeout(IO_TIMEOUT, Arc::clone(&self.io_slot).acquire_owned())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "playback cache inventory slot is busy",
                )
            })?
            .map_err(io::Error::other)?;
        let roots = self.roots.clone();
        #[cfg(test)]
        let override_space = *self.test_space.lock().expect("test space");
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut sample = inventory(&roots)?;
            sample.committed = committed;
            sample.device_committed = device_committed;
            #[cfg(test)]
            if let Some(space) = override_space {
                sample.disks.values_mut().for_each(|value| *value = space);
            }
            Ok::<_, io::Error>(sample)
        });
        let sample = tokio::time::timeout(IO_TIMEOUT, task)
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "playback cache inventory timed out",
                )
            })?
            .map_err(io::Error::other)??;
        let mut state = self.lock();
        // A cancelled reservation may have left partial files after the scan
        // began. Its dirty marker must survive until a later complete scan.
        // Compare a generation rather than clearing another owner's signal.
        state.sample = Some(sample);
        state.dirty = state.dirty_revision != dirty_revision;
        Ok(())
    }

    async fn reserve_owned(
        self: &Arc<Self>,
        path: &Path,
        bytes: u64,
    ) -> io::Result<CacheReservation> {
        let path = normalized(path)?;
        let index = self.root_index(&path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is outside playback cache roots",
            )
        })?;
        let _operation = self.operation().await?;
        self.refresh(false).await?;
        let (device, allocation_unit) = {
            let state = self.lock();
            let sample = state
                .sample
                .as_ref()
                .ok_or_else(|| io::Error::other("playback cache inventory unavailable"))?;
            let device = sample.root_devices[index];
            (device, sample.disks[&device].allocation_unit)
        };
        let bytes = allocation_charge(bytes, allocation_unit);
        if bytes > self.lock().ceiling {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "write exceeds shared playback cache budget",
            ));
        }
        self.make_room(Some((device, bytes))).await?;
        let mut state = self.lock();
        if state.dirty || state.ceiling == 0 || state.remaining(device) < bytes {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "shared playback cache budget or filesystem reserve exhausted",
            ));
        }
        if state
            .evicting
            .values()
            .any(|candidate| overlapping(candidate, &path))
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "playback cache entry is being evicted",
            ));
        }
        let id = state.id();
        state.pending.insert(
            id,
            Pending {
                bytes,
                device,
                allocation_unit,
            },
        );
        state.pins.insert(id, path);
        Ok(CacheReservation {
            budget: Arc::clone(self),
            id,
            settled: false,
        })
    }

    pub(crate) async fn available_bytes(&self, path: &Path) -> io::Result<u64> {
        let path = normalized(path)?;
        let index = self.root_index(&path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is outside playback cache roots",
            )
        })?;
        let _operation = self.operation().await?;
        self.refresh(false).await?;
        let state = self.lock();
        let device = state
            .sample
            .as_ref()
            .ok_or_else(|| io::Error::other("playback cache inventory unavailable"))?
            .root_devices[index];
        Ok(if state.dirty {
            0
        } else {
            state.remaining(device)
        })
    }

    pub(crate) async fn ceiling_bytes(&self) -> io::Result<u64> {
        let _operation = self.operation().await?;
        self.refresh(false).await?;
        Ok(self.lock().ceiling)
    }

    /// Used by settings and queue admission; includes old files and every
    /// outstanding writer grant, regardless of the namespace's Store rows.
    pub(crate) async fn used_bytes(&self) -> io::Result<u64> {
        let _operation = self.operation().await?;
        self.refresh(false).await?;
        let state = self.lock();
        if state.dirty {
            return Err(io::Error::other(
                "playback cache inventory changed while scanning",
            ));
        }
        Ok(state.used())
    }

    #[cfg(test)]
    pub(crate) fn configure_for_test(&self, ceiling: u64) {
        self.test_ceiling
            .store(ceiling, std::sync::atomic::Ordering::Relaxed);
        *self.test_space.lock().expect("test space") = Some(DiskSpace {
            free: 1024 * GIB,
            total: 1024 * GIB,
            allocation_unit: 1,
        });
        let mut state = self.lock();
        state.setting_at = None;
        state.dirty = true;
        state.dirty_revision = state.dirty_revision.wrapping_add(1);
    }

    async fn make_room(self: &Arc<Self>, request: Option<(u64, u64)>) -> io::Result<SweepReport> {
        let mut report = SweepReport {
            bytes_before: self.lock().used(),
            ..SweepReport::default()
        };
        if !self.lock().pressure(request) {
            report.bytes_after = report.bytes_before;
            return Ok(report);
        }
        let deadline = tokio::time::Instant::now() + PRESSURE_TIMEOUT;
        let owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut candidates = Vec::new();
        for owner in owners {
            let until = deadline.min(tokio::time::Instant::now() + IO_TIMEOUT);
            let permit =
                tokio::time::timeout_at(until, Arc::clone(&self.callback_slot).acquire_owned())
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "playback cache owner is busy")
                    })?
                    .map_err(io::Error::other)?;
            let scanner = Arc::clone(&owner);
            let task = tokio::spawn(async move {
                let _permit = permit;
                scanner.candidates().await
            });
            let listed = tokio::time::timeout_at(until, task)
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "playback cache candidate inventory timed out",
                    )
                })?
                .map_err(io::Error::other)??;
            for mut candidate in listed {
                candidate.path = normalized(&candidate.path)?;
                if self.root_index(&candidate.path).is_some() {
                    candidates.push((candidate, Arc::clone(&owner)));
                }
            }
        }
        candidates.sort_by(|(first, _), (second, _)| {
            first
                .last_used
                .cmp(&second.last_used)
                .then_with(|| first.path.cmp(&second.path))
        });
        for (candidate, owner) in candidates {
            let until = deadline.min(tokio::time::Instant::now() + IO_TIMEOUT);
            let permit =
                tokio::time::timeout_at(until, Arc::clone(&self.callback_slot).acquire_owned())
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "playback cache owner is busy")
                    })?
                    .map_err(io::Error::other)?;
            let id = {
                let mut state = self.lock();
                if !state.pressure(request) {
                    break;
                }
                // When only one filesystem is short, deleting an unrelated
                // namespace on another device cannot repair its headroom.
                let index = self.root_index(&candidate.path).expect("managed candidate");
                let candidate_device =
                    state.sample.as_ref().expect("inventory").root_devices[index];
                let global_pressure = state
                    .used()
                    .saturating_add(request.map_or(0, |(_, bytes)| bytes))
                    > state.ceiling;
                if !global_pressure {
                    if let Some((device, _)) = request {
                        if device != candidate_device {
                            continue;
                        }
                    } else {
                        let sample = state.sample.as_ref().expect("inventory");
                        let space = sample.disks[&candidate_device];
                        let pending = state
                            .pending
                            .values()
                            .filter(|pending| pending.device == candidate_device)
                            .fold(0u64, |sum, pending| sum.saturating_add(pending.bytes));
                        let committed = state
                            .device_committed
                            .get(&candidate_device)
                            .copied()
                            .unwrap_or(0)
                            .saturating_sub(
                                sample
                                    .device_committed
                                    .get(&candidate_device)
                                    .copied()
                                    .unwrap_or(0),
                            );
                        if space.free
                            >= space
                                .floor()
                                .saturating_add(pending)
                                .saturating_add(narrow(committed))
                        {
                            continue;
                        }
                    }
                }
                if state
                    .pins
                    .values()
                    .chain(state.evicting.values())
                    .any(|path| overlapping(path, &candidate.path))
                {
                    report.protected += 1;
                    continue;
                }
                let id = state.id();
                state.evicting.insert(id, candidate.path.clone());
                id
            };
            let eviction = CacheEvictionGuard {
                budget: Arc::clone(self),
                id,
                settled: false,
            };
            // The detached owner keeps the fence if an admission request is
            // cancelled. Dropping its awaiter must never expose a namespace
            // while the callback still has an in-flight unlink.
            let task = tokio::spawn(async move {
                let _permit = permit;
                let result = owner.evict(&candidate).await;
                (result, eviction, candidate)
            });
            let (result, mut eviction, candidate) = tokio::time::timeout_at(until, task)
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "playback cache eviction timed out")
                })?
                .map_err(io::Error::other)?;
            match result {
                Ok(true) => report.evicted += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, path = %candidate.path.display(), "playback cache eviction failed; keeping its charge")
                }
            }
            // Never infer physical credit from a logical size or an unlink:
            // another namespace may hold a hard link to the same allocation.
            self.refresh(true).await?;
            eviction.settled = true;
            drop(eviction);
        }
        report.bytes_after = self.lock().used();
        Ok(report)
    }

    pub(crate) async fn sweep(self: &Arc<Self>) -> io::Result<SweepReport> {
        let _operation = self.operation().await?;
        self.refresh(true).await?;
        self.make_room(None).await
    }

    pub(crate) async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! { biased;
                _ = shutdown.cancelled() => return,
                _ = interval.tick() => {},
            }
            let sweep = self.sweep();
            tokio::select! { biased;
                _ = shutdown.cancelled() => return,
                result = sweep => match result {
                    Ok(report) if report.evicted > 0 || report.bytes_after > self.lock().ceiling => tracing::info!(?report, "shared playback cache sweep"),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(%error, "shared playback cache inventory unavailable; new writes remain closed"),
                },
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct CachePin {
    budget: Arc<PlaybackCacheBudget>,
    id: u64,
}
impl Drop for CachePin {
    fn drop(&mut self) {
        self.budget.lock().pins.remove(&self.id);
    }
}

#[derive(Debug)]
pub(crate) struct CacheReservation {
    budget: Arc<PlaybackCacheBudget>,
    id: u64,
    settled: bool,
}
impl CacheReservation {
    /// Settles a bounded write. Zero is useful when the operation only created
    /// a hard link; inventory still decides physical credit after eviction.
    pub(crate) fn commit(mut self, actual_bytes: u64) {
        let mut state = self.budget.lock();
        if let Some(pending) = state.pending.remove(&self.id) {
            let actual_bytes = allocation_charge(actual_bytes, pending.allocation_unit);
            state.committed = state.committed.saturating_add(u128::from(actual_bytes));
            let committed = state.device_committed.entry(pending.device).or_default();
            *committed = committed.saturating_add(u128::from(actual_bytes));
        }
        state.pins.remove(&self.id);
        self.settled = true;
    }
}
impl Drop for CacheReservation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut state = self.budget.lock();
        state.pending.remove(&self.id);
        state.pins.remove(&self.id);
        state.dirty = true;
        state.dirty_revision = state.dirty_revision.wrapping_add(1);
    }
}

#[derive(Debug)]
pub(crate) struct CacheEvictionGuard {
    budget: Arc<PlaybackCacheBudget>,
    id: u64,
    settled: bool,
}
impl Drop for CacheEvictionGuard {
    fn drop(&mut self) {
        let mut state = self.budget.lock();
        state.evicting.remove(&self.id);
        // Also covers cancellation while an owner's unlink future was running.
        if !self.settled {
            state.dirty = true;
            state.dirty_revision = state.dirty_revision.wrapping_add(1);
        }
    }
}

fn inventory(roots: &[PathBuf]) -> io::Result<Sample> {
    let mut sample = Sample {
        at: Instant::now(),
        bytes: 0,
        disks: HashMap::new(),
        root_devices: Vec::new(),
        committed: 0,
        device_committed: HashMap::new(),
    };
    let mut seen = HashSet::new();
    for root in roots {
        let mut existing = root.as_path();
        let directory = loop {
            match plurx_core::fs_secure::open_directory_nofollow_blocking(existing) {
                Ok(directory) => break directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    existing = existing.parent().ok_or(error)?;
                }
                Err(error) => return Err(error),
            }
        };
        let identity = plurx_core::fs_secure::std_file_identity(&directory)?;
        sample.root_devices.push(identity.device);
        sample
            .disks
            .entry(identity.device)
            .or_insert(disk_space(&directory, existing)?);
        if existing == root {
            sample.bytes = sample
                .bytes
                .saturating_add(walk(&directory, root, &mut seen, 0)?);
        }
    }
    Ok(sample)
}

#[cfg(unix)]
fn disk_space(directory: &std::fs::File, _path: &Path) -> io::Result<DiskSpace> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the held descriptor and writable statvfs output remain live.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    let fragment = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    } as u128;
    Ok(DiskSpace {
        free: narrow((stat.f_bavail as u128).saturating_mul(fragment)),
        total: narrow((stat.f_blocks as u128).saturating_mul(fragment)),
        allocation_unit: narrow(u128::from(stat.f_frsize.max(stat.f_bsize))).max(1),
    })
}

#[cfg(windows)]
fn disk_space(_directory: &std::fs::File, path: &Path) -> io::Result<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut free = 0;
    let mut total = 0;
    // SAFETY: terminated input and all writable outputs remain live.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &raw mut free,
            &raw mut total,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // NTFS/ReFS allocation can exceed the logical sector size. A 64 KiB
    // fallback conservatively covers the standard Windows allocation units.
    Ok(DiskSpace {
        free,
        total,
        allocation_unit: 64 * 1024,
    })
}

#[cfg(unix)]
fn clear_errno() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: libc returns this thread's writable errno slot.
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    // SAFETY: libc returns this thread's writable errno slot.
    unsafe {
        *libc::__error() = 0;
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn walk(
    directory: &std::fs::File,
    _path: &Path,
    seen: &mut HashSet<(u64, u64)>,
    depth: usize,
) -> io::Result<u64> {
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;
    if depth > 256 {
        return Err(io::Error::other(
            "playback cache inventory exceeded depth bound",
        ));
    }
    let metadata = directory.metadata()?;
    if !seen.insert((metadata.dev(), metadata.ino())) {
        return Ok(0);
    }
    let mut bytes = metadata.blocks().saturating_mul(512);
    let dot = CString::new(".").expect("static dot");
    // SAFETY: openat receives a live directory and terminated static name.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership of the new descriptor on success.
    let entries = unsafe { libc::fdopendir(fd) };
    if entries.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: failed fdopendir left ownership with this function.
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: this guard exclusively owns the live directory stream.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let _stream = DirectoryStream(entries);
    loop {
        clear_errno();
        // SAFETY: stream is owned until _stream drops; no concurrent cursor.
        let entry = unsafe { libc::readdir(entries) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(error);
            }
            break;
        }
        // SAFETY: readdir returned a live entry, valid until its next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: child is terminated; fstatat never follows its final symlink.
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(error);
        }
        // SAFETY: successful fstatat initialized all fields.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            // SAFETY: secure child traversal relative to the held parent.
            let child = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            };
            if child < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: successful openat returned a new exclusively owned fd.
            let child = unsafe { std::fs::File::from_raw_fd(child) };
            bytes = bytes.saturating_add(walk(&child, _path, seen, depth + 1)?);
        } else if seen.insert((stat.st_dev as u64, stat.st_ino as u64)) {
            bytes = bytes.saturating_add((stat.st_blocks.max(0) as u64).saturating_mul(512));
        }
    }
    Ok(bytes)
}

#[cfg(windows)]
fn allocated_bytes(file: &std::fs::File) -> io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
    };
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: the held file and correctly sized writable output remain live.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileStandardInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.AllocationSize.max(0) as u64)
}

#[cfg(windows)]
fn walk(
    directory: &std::fs::File,
    path: &Path,
    seen: &mut HashSet<(u64, u64)>,
    depth: usize,
) -> io::Result<u64> {
    if depth > 256 {
        return Err(io::Error::other(
            "playback cache inventory exceeded depth bound",
        ));
    }
    let identity = plurx_core::fs_secure::std_file_identity(directory)?;
    if !seen.insert((identity.device, identity.inode)) {
        return Ok(0);
    }
    let mut bytes = allocated_bytes(directory)?;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let held = if metadata.is_dir() {
            plurx_core::fs_secure::open_directory_nofollow_blocking(&entry.path())?
        } else {
            plurx_core::fs_secure::open_read_nofollow_blocking(&entry.path())?
        };
        if held.metadata()?.is_dir() {
            bytes = bytes.saturating_add(walk(&held, &entry.path(), seen, depth + 1)?);
        } else {
            let identity = plurx_core::fs_secure::std_file_identity(&held)?;
            if seen.insert((identity.device, identity.inode)) {
                bytes = bytes.saturating_add(allocated_bytes(&held)?);
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::store::{SettingsStore, SqliteStore};
    use std::sync::atomic::Ordering;

    struct Files {
        candidates: Vec<CacheCandidate>,
        failures: HashSet<PathBuf>,
        removed: Mutex<Vec<PathBuf>>,
    }

    impl Files {
        fn new(paths: Vec<PathBuf>) -> Arc<Self> {
            Arc::new(Self {
                candidates: paths
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| CacheCandidate {
                        path,
                        last_used: SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64),
                    })
                    .collect(),
                failures: HashSet::new(),
                removed: Mutex::default(),
            })
        }
    }

    #[async_trait::async_trait]
    impl CacheOwner for Files {
        async fn candidates(&self) -> io::Result<Vec<CacheCandidate>> {
            Ok(self
                .candidates
                .iter()
                .filter(|candidate| candidate.path.exists())
                .cloned()
                .collect())
        }
        async fn evict(&self, candidate: &CacheCandidate) -> io::Result<bool> {
            if self.failures.contains(&candidate.path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected unlink failure",
                ));
            }
            if candidate.path.is_dir() {
                tokio::fs::remove_dir_all(&candidate.path).await?;
            } else {
                tokio::fs::remove_file(&candidate.path).await?;
            }
            self.removed
                .lock()
                .expect("removed")
                .push(candidate.path.clone());
            Ok(true)
        }
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        roots: Vec<PathBuf>,
        store: Arc<SqliteStore>,
        budget: Arc<PlaybackCacheBudget>,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("temporary");
            // macOS's temporary directory may be reached through /var.
            let base = std::fs::canonicalize(temporary.path()).expect("canonical temp");
            let roots = ["renditions", "pretranscodes", "subtitle-source"]
                .into_iter()
                .map(|name| {
                    let root = base.join(name);
                    std::fs::create_dir(&root).expect("cache namespace");
                    root
                })
                .collect::<Vec<_>>();
            let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let budget = PlaybackCacheBudget::new(store.clone(), roots.clone());
            *budget.test_space.lock().expect("space") = Some(DiskSpace {
                free: 1024 * GIB,
                total: 1024 * GIB,
                allocation_unit: 1,
            });
            Self {
                _temporary: temporary,
                roots,
                store,
                budget,
            }
        }

        fn file(&self, root: usize, name: &str, bytes: usize) -> PathBuf {
            let path = self.roots[root].join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
            std::fs::write(&path, vec![0x5a; bytes]).expect("file");
            path
        }

        fn limit(&self, bytes: u64) {
            self.budget.test_ceiling.store(bytes, Ordering::Relaxed);
            self.budget.lock().setting_at = None;
        }

        async fn used(&self) -> u64 {
            self.budget.sweep().await.expect("inventory").bytes_after
        }
    }

    #[tokio::test]
    async fn one_budget_evicts_global_lru_across_playback_namespaces() {
        let fixture = Fixture::new();
        let rendition = fixture.file(0, "old.m4s", 64 * 1024);
        let subtitle = fixture.file(2, "next.raw", 64 * 1024);
        let transcode = fixture.file(1, "recent.ts", 64 * 1024);
        let used = fixture.used().await;
        fixture.limit(used - 32 * 1024);
        // Registration order deliberately disagrees with LRU order.
        let newer = Files::new(vec![subtitle.clone(), transcode.clone()]);
        let older = Files::new(vec![rendition.clone()]);
        let newer = Arc::new(Files {
            candidates: newer
                .candidates
                .iter()
                .cloned()
                .map(|mut entry| {
                    entry.last_used += Duration::from_secs(10);
                    entry
                })
                .collect(),
            failures: HashSet::new(),
            removed: Mutex::default(),
        });
        fixture.budget.register(newer.clone());
        fixture.budget.register(older.clone());
        let report = fixture.budget.sweep().await.expect("sweep");
        assert_eq!(report.evicted, 1);
        assert!(!rendition.exists());
        assert!(subtitle.exists() && transcode.exists());
        assert_eq!(*older.removed.lock().expect("removed"), vec![rendition]);
        assert!(newer.removed.lock().expect("removed").is_empty());
        assert!(report.bytes_after <= fixture.budget.lock().ceiling);
    }

    #[tokio::test]
    async fn reservations_evict_other_namespaces_and_charge_committed_writes() {
        let fixture = Fixture::new();
        let old = fixture.file(0, "old.m4s", 64 * 1024);
        let used = fixture.used().await;
        fixture.limit(used + 32 * 1024);
        fixture.budget.register(Files::new(vec![old.clone()]));
        let output = fixture.roots[2].join("new.raw");
        let reservation = reserve(&output, 64 * 1024)
            .await
            .expect("admitted")
            .expect("managed");
        assert!(!old.exists());
        assert!(is_pinned(&output));
        std::fs::write(&output, vec![0x5a; 64 * 1024]).expect("write");
        let pending = fixture.budget.lock().used();
        reservation.commit(64 * 1024);
        assert_eq!(fixture.budget.lock().used(), pending);
        assert!(!is_pinned(&output));
        assert!(reserve(&fixture.roots[1].join("oversubscribed"), 64 * 1024)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn tiny_writes_charge_whole_filesystem_allocation_units_between_scans() {
        let fixture = Fixture::new();
        let actual = inventory(&fixture.roots).expect("real filesystem inventory");
        let unit = actual
            .disks
            .values()
            .next()
            .expect("filesystem")
            .allocation_unit;
        assert!(unit > 1, "real filesystem allocation unit");
        *fixture.budget.test_space.lock().expect("space") = Some(DiskSpace {
            free: 1024 * GIB,
            total: 1024 * GIB,
            allocation_unit: unit,
        });
        let baseline = fixture.used().await;
        fixture.limit(baseline + 3 * unit);
        for index in 0..3 {
            let path = fixture.roots[index].join("one-byte");
            let reservation = reserve(&path, 1)
                .await
                .expect("one allocation")
                .expect("managed");
            assert_eq!(
                fixture
                    .budget
                    .lock()
                    .pending
                    .values()
                    .next()
                    .expect("pending grant")
                    .bytes,
                unit
            );
            std::fs::write(&path, b"x").expect("one-byte file");
            reservation.commit(1);
        }
        assert_eq!(fixture.budget.lock().used(), baseline + 3 * unit);
        assert!(reserve(&fixture.roots[0].join("overflow"), 1)
            .await
            .is_err());
        let before = fixture.budget.lock().used();
        reserve(&fixture.roots[0].join("link-only"), 0)
            .await
            .expect("no new allocation")
            .expect("managed")
            .commit(0);
        assert_eq!(
            fixture.budget.lock().used(),
            before,
            "hard-link-only commits stay zero"
        );
    }

    #[tokio::test]
    async fn stalled_owner_inventory_is_bounded_and_does_not_spawn_more_callbacks() {
        struct HangingInventory {
            started: tokio::sync::Notify,
            proceed: tokio::sync::Notify,
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait::async_trait]
        impl CacheOwner for HangingInventory {
            async fn candidates(&self) -> io::Result<Vec<CacheCandidate>> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                self.started.notify_one();
                self.proceed.notified().await;
                Ok(Vec::new())
            }
            async fn evict(&self, _candidate: &CacheCandidate) -> io::Result<bool> {
                unreachable!()
            }
        }
        let fixture = Fixture::new();
        fixture.file(0, "old", 8192);
        fixture.used().await;
        fixture.budget.lock().ceiling = 0;
        let owner = Arc::new(HangingInventory {
            started: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        fixture.budget.register(owner.clone());
        let budget = Arc::clone(&fixture.budget);
        let first = tokio::spawn(async move { budget.make_room(None).await });
        owner.started.notified().await;
        tokio::time::pause();
        tokio::time::advance(IO_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(
            first
                .await
                .expect("pressure task")
                .expect_err("owner timeout")
                .kind(),
            io::ErrorKind::TimedOut
        );
        let budget = Arc::clone(&fixture.budget);
        let second = tokio::spawn(async move { budget.make_room(None).await });
        tokio::task::yield_now().await;
        tokio::time::advance(IO_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(
            second
                .await
                .expect("retry task")
                .expect_err("single owner slot remains busy")
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            owner.calls.load(Ordering::Relaxed),
            1,
            "timed-out owner work never accumulates"
        );
        tokio::time::resume();
        owner.proceed.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture.budget.callback_slot.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached inventory releases callback slot");
    }

    #[tokio::test]
    async fn concurrent_reservations_cannot_spend_one_allowance_twice() {
        let fixture = Fixture::new();
        let baseline = fixture.used().await;
        fixture.limit(baseline + 12 * 1024);
        let first = fixture.roots[0].join("first");
        let second = fixture.roots[2].join("second");
        let (first, second) = tokio::join!(reserve(&first, 8 * 1024), reserve(&second, 8 * 1024));
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(fixture.budget.lock().pending.len(), 1);
    }

    #[tokio::test]
    async fn child_reader_pins_parent_candidate_and_parent_pin_protects_children() {
        let fixture = Fixture::new();
        let file = fixture.file(0, "title/segment.m4s", 64 * 1024);
        let directory = file.parent().expect("title").to_path_buf();
        let used = fixture.used().await;
        fixture.limit(used - 32 * 1024);
        fixture.budget.register(Files::new(vec![directory.clone()]));
        let reader = pin(&file).expect("pin").expect("managed");
        assert!(is_pinned(&directory));
        let report = fixture.budget.sweep().await.expect("protected sweep");
        assert_eq!(report.protected, 1);
        assert!(file.exists());
        drop(reader);
        let parent = pin(&fixture.roots[0])
            .expect("parent pin")
            .expect("managed");
        assert!(is_pinned(&file));
        assert_eq!(
            fixture
                .budget
                .sweep()
                .await
                .expect("parent sweep")
                .protected,
            1
        );
        drop(parent);
        assert_eq!(
            fixture
                .budget
                .sweep()
                .await
                .expect("unpinned sweep")
                .evicted,
            1
        );
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn pins_fail_while_parent_eviction_owns_namespace() {
        struct Paused {
            candidate: CacheCandidate,
            entered: tokio::sync::Notify,
            proceed: tokio::sync::Notify,
        }
        #[async_trait::async_trait]
        impl CacheOwner for Paused {
            async fn candidates(&self) -> io::Result<Vec<CacheCandidate>> {
                Ok(vec![self.candidate.clone()])
            }
            async fn evict(&self, candidate: &CacheCandidate) -> io::Result<bool> {
                self.entered.notify_one();
                self.proceed.notified().await;
                tokio::fs::remove_dir_all(&candidate.path).await?;
                Ok(true)
            }
        }
        let fixture = Fixture::new();
        let file = fixture.file(0, "title/segment.m4s", 64 * 1024);
        let used = fixture.used().await;
        fixture.limit(used - 32 * 1024);
        let owner = Arc::new(Paused {
            candidate: CacheCandidate {
                path: file.parent().expect("parent").to_owned(),
                last_used: SystemTime::UNIX_EPOCH,
            },
            entered: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
        });
        fixture.budget.register(owner.clone());
        let budget = Arc::clone(&fixture.budget);
        let sweep = tokio::spawn(async move { budget.sweep().await });
        owner.entered.notified().await;
        assert_eq!(
            pin(&file).expect_err("eviction excludes reader").kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            pin(&fixture.roots[0])
                .expect_err("eviction excludes parent reader")
                .kind(),
            io::ErrorKind::WouldBlock
        );
        tokio::time::pause();
        tokio::time::advance(IO_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(
            sweep
                .await
                .expect("sweep task")
                .expect_err("bounded eviction wait")
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            pin(&file)
                .expect_err("detached owner keeps its fence")
                .kind(),
            io::ErrorKind::WouldBlock
        );
        tokio::time::resume();
        owner.proceed.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if pin(&file).is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owner finishes and releases fence");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn independent_cleanup_fence_and_reader_are_mutually_exclusive() {
        let fixture = Fixture::new();
        let file = fixture.file(0, "title/segment.m4s", 8192);
        fixture.used().await;
        let directory = file.parent().expect("parent");
        let reader = pin(&file).expect("read pin").expect("managed");
        assert_eq!(
            begin_eviction(directory).expect_err("reader wins").kind(),
            io::ErrorKind::WouldBlock
        );
        drop(reader);
        let fence = begin_eviction(directory)
            .expect("cleanup fence")
            .expect("managed");
        assert_eq!(
            pin(&file).expect_err("cleanup wins").kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            begin_eviction(&file).expect_err("overlapping fence").kind(),
            io::ErrorKind::WouldBlock
        );
        drop(fence);
        assert!(
            fixture.budget.lock().dirty,
            "partial deletion must be reconciled"
        );
        assert!(pin(&file).expect("fence released").is_some());
    }

    #[tokio::test]
    async fn cancelled_write_reconciles_partial_file_before_reusing_capacity() {
        let fixture = Fixture::new();
        let baseline = fixture.used().await;
        fixture.limit(baseline + 64 * 1024);
        let output = fixture.roots[2].join("partial.tmp");
        let reservation = reserve(&output, 64 * 1024)
            .await
            .expect("reserve")
            .expect("managed");
        std::fs::write(output, vec![0x5a; 64 * 1024]).expect("partial write");
        drop(reservation);
        assert!(fixture.budget.lock().dirty);
        let error = reserve(&fixture.roots[0].join("next"), 64 * 1024)
            .await
            .expect_err("partial bytes remain charged");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(fixture.budget.lock().used() >= baseline + 64 * 1024);
    }

    #[tokio::test]
    async fn hidden_and_restart_files_are_charged_before_first_admission() {
        let fixture = Fixture::new();
        fixture.file(0, ".old-generation/segment.m4s", 64 * 1024);
        fixture.file(2, ".failed-source.tmp", 64 * 1024);
        fixture.limit(64 * 1024);
        assert!(fixture.budget.lock().sample.is_none());
        let error = reserve(&fixture.roots[1].join("new"), 1)
            .await
            .expect_err("old data fills budget");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(fixture.budget.lock().used() >= 128 * 1024);
    }

    #[tokio::test]
    async fn disabling_or_shrinking_budget_evicts_existing_idle_data() {
        let fixture = Fixture::new();
        let old = fixture.file(0, "old", 64 * 1024);
        let new = fixture.file(2, "new", 64 * 1024);
        fixture
            .budget
            .register(Files::new(vec![old.clone(), new.clone()]));
        let baseline = fixture.used().await;
        fixture.limit(baseline - 32 * 1024);
        assert_eq!(
            fixture.budget.sweep().await.expect("shrink sweep").evicted,
            1
        );
        assert!(!old.exists() && new.exists());
        fixture
            .store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("disable cache");
        assert_eq!(fixture.budget.sweep().await.expect("off sweep").evicted, 1);
        assert!(!new.exists());
        assert_eq!(
            fixture
                .budget
                .available_bytes(&fixture.roots[0])
                .await
                .expect("available"),
            0
        );
        assert!(reserve(&fixture.roots[0].join("disabled"), 1)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn failed_unlink_never_returns_budget_credit() {
        let fixture = Fixture::new();
        let file = fixture.file(0, "failed", 64 * 1024);
        let baseline = fixture.used().await;
        let mut owner = Arc::try_unwrap(Files::new(vec![file.clone()]))
            .ok()
            .expect("sole owner");
        owner.failures.insert(file.clone());
        fixture.budget.register(Arc::new(owner));
        fixture.limit(baseline - 32 * 1024);
        let report = fixture.budget.sweep().await.expect("failed removal sweep");
        assert_eq!(report.evicted, 0);
        assert_eq!(report.bytes_after, baseline);
        assert!(file.exists());
        assert!(reserve(&fixture.roots[2].join("new"), 1).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hardlink_eviction_frees_credit_only_after_last_namespace_unlinks() {
        use std::os::unix::fs::MetadataExt;
        let fixture = Fixture::new();
        let first = fixture.file(0, "first", 128 * 1024);
        let second = fixture.roots[2].join("second");
        std::fs::hard_link(&first, &second).expect("cross namespace hardlink");
        let allocated = std::fs::metadata(&first).expect("metadata").blocks() * 512;
        assert!(allocated >= 64 * 1024);
        fixture
            .budget
            .register(Files::new(vec![first.clone(), second.clone()]));
        let baseline = fixture.used().await;
        assert!(baseline < allocated * 2, "hardlink counted once");
        fixture.limit(baseline - allocated / 2);
        let report = fixture.budget.sweep().await.expect("hardlink sweep");
        assert_eq!(
            report.evicted, 2,
            "first unlink cannot free the still linked data"
        );
        assert!(!first.exists() && !second.exists());
        assert!(report.bytes_after <= fixture.budget.lock().ceiling);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inventory_does_not_follow_symlinks_and_failed_scan_closes_admission() {
        let fixture = Fixture::new();
        let outside = fixture._temporary.path().join("outside");
        std::fs::write(&outside, vec![0x5a; 1024 * 1024]).expect("outside");
        std::os::unix::fs::symlink(&outside, fixture.roots[0].join("link")).expect("symlink");
        assert!(fixture.used().await < 1024 * 1024);
        std::fs::remove_file(fixture.roots[0].join("link")).expect("unlink");
        std::fs::remove_dir(&fixture.roots[0]).expect("remove root");
        std::os::unix::fs::symlink(fixture.roots[1].clone(), &fixture.roots[0])
            .expect("root symlink");
        assert!(fixture.budget.sweep().await.is_err());
        assert!(reserve(&fixture.roots[2].join("blocked"), 1).await.is_err());
        assert!(fixture.budget.lock().dirty);
    }

    #[tokio::test]
    async fn filesystem_floor_and_pending_grants_override_configured_space() {
        let fixture = Fixture::new();
        // At 100 GiB, two percent is stricter than the minimum 1 GiB floor.
        *fixture.budget.test_space.lock().expect("space") = Some(DiskSpace {
            free: 2 * GIB + 4096,
            total: 100 * GIB,
            allocation_unit: 1,
        });
        let first = fixture.roots[0].join("first");
        assert!(reserve(&first, 8192).await.is_err());
        let grant = reserve(&first, 4096)
            .await
            .expect("last safe bytes")
            .expect("managed");
        assert_eq!(
            fixture
                .budget
                .available_bytes(&fixture.roots[2])
                .await
                .expect("same device"),
            0
        );
        assert!(reserve(&fixture.roots[2].join("second"), 1).await.is_err());
        drop(grant);
        *fixture.budget.test_space.lock().expect("space") = Some(DiskSpace {
            free: GIB + 4096,
            total: 10 * GIB,
            allocation_unit: 1,
        });
        fixture.budget.sweep().await.expect("refresh minimum floor");
        assert!(reserve(&first, 8192).await.is_err());
    }
}
