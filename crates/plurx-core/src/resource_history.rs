//! Bounded node-instance-local service-cost priors. Never a reservation,
//! executable proof, peak allocation bound, or extrapolated concurrency permit.
//! Blocking filesystem work belongs on the daemon's blocking I/O owner, outside
//! scheduler locks. The lifetime lock survives every synchronous atomic save.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAX_CLASSES: usize = 1024;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const PRIOR_LIFETIME_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const RECENT_LIFETIME: Duration = Duration::from_secs(15);

/// Canonical adapter digests include helper/executable hashes, device/driver/API
/// and effective limits in runtime_epoch; graph stages, geometry, fps, HDR,
/// codec, audio/subtitles in workload; exact recipe/source in recipe; and the
/// ACTUAL concurrently observed mix in concurrent_mix. No N-from-one inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryKey {
    pub runtime_epoch: [u8; 32],
    pub workload: [u8; 32],
    pub recipe: [u8; 32],
    pub concurrent_mix: [u8; 32],
    pub phase: CostPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostPhase {
    Startup,
    Steady,
}

#[derive(Clone, Copy, Debug)]
pub struct CostSample {
    /// Caller supplies monotonic time and its wall-time receipt together.
    pub at: Instant,
    pub unix_ms: u64,
    pub cpu_ns: u64,
    pub completed_units: u64,
    pub elapsed_ns: u64,
    /// Observed allocation, emphatically not a safe upper bound.
    pub observed_resident_bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CostSummary {
    pub samples: u32,
    pub max_cpu_ns_per_unit: u64,
    pub max_elapsed_ns_per_unit: u64,
    pub max_observed_resident_bytes: u64,
    pub last_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryStorage {
    Persistent,
    LockConflictVolatile,
    UnavailableVolatile,
    UnrecognizedVolatile,
}

/// Observed service costs for operator inspection. These are neither hard
/// allocation ceilings nor a claim about unseen concurrent workloads.
#[derive(Serialize)]
pub struct HistoryDiagnostics {
    pub storage: HistoryStorage,
    pub classes: usize,
    pub recent_classes: usize,
    pub latest: Option<ObservedCost>,
}
#[derive(Serialize)]
pub struct ObservedCost {
    pub phase: CostPhase,
    pub summary: CostSummary,
    /// Persisted priors have no monotonic stamp after restart.
    pub age_ms: crate::process::observation::Metric<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    schema: u32,
    node_instance: uuid::Uuid,
    entries: Vec<(HistoryKey, CostSummary)>,
}

struct Entry {
    summary: CostSummary,
    last_monotonic: Option<Instant>,
    lru: u64,
}

struct Persistence {
    directory: PathBuf,
    // Never truncate, unlink or replace the inode which carries this lock.
    _lock: File,
}

pub struct ResourceHistory {
    node_instance: uuid::Uuid,
    storage: HistoryStorage,
    persistence: Option<Persistence>,
    entries: BTreeMap<HistoryKey, Entry>,
    clock: u64,
}

impl ResourceHistory {
    /// Instance IDs are typed UUIDs, not untrusted path components. A second
    /// owner cannot read the first daemon's records as its own or persist over
    /// them. Unknown schema/corrupt files remain untouched for diagnosis.
    pub fn open(data_directory: &Path, node_instance: uuid::Uuid, unix_ms: u64) -> Self {
        let mut history = Self {
            node_instance,
            storage: HistoryStorage::UnavailableVolatile,
            persistence: None,
            entries: BTreeMap::new(),
            clock: 0,
        };
        if node_instance.is_nil() {
            return history;
        }
        let directory = data_directory
            .join("resource-history")
            .join(node_instance.simple().to_string());
        let Ok(lease) = open_persistence(&directory) else {
            return history;
        };
        let Some(lease) = lease else {
            history.storage = HistoryStorage::LockConflictVolatile;
            return history;
        };
        match read_image(&directory.join("summaries.json")) {
            Ok(None) => {}
            Ok(Some(image))
                if image.schema == 1
                    && image.node_instance == node_instance
                    && image.entries.len() <= MAX_CLASSES =>
            {
                for (key, summary) in image.entries {
                    if !valid_key(key)
                        || summary.samples == 0
                        || summary.last_unix_ms > unix_ms
                        || history.entries.contains_key(&key)
                    {
                        history.entries.clear();
                        history.storage = HistoryStorage::UnrecognizedVolatile;
                        return history;
                    }
                    if unix_ms - summary.last_unix_ms <= PRIOR_LIFETIME_MS {
                        history.clock += 1;
                        history.entries.insert(
                            key,
                            Entry {
                                summary,
                                last_monotonic: None,
                                lru: history.clock,
                            },
                        );
                    }
                }
            }
            _ => {
                history.storage = HistoryStorage::UnrecognizedVolatile;
                return history;
            }
        }
        history.storage = HistoryStorage::Persistent;
        history.persistence = Some(lease);
        history
    }

    pub fn storage(&self) -> HistoryStorage {
        self.storage
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn diagnostics(&self, now: Instant) -> HistoryDiagnostics {
        use crate::process::observation::{Metric, UnknownReason};
        let age = |entry: &Entry| {
            entry
                .last_monotonic
                .and_then(|at| now.checked_duration_since(at))
        };
        let latest = self
            .entries
            .iter()
            .max_by_key(|(_, entry)| (entry.summary.last_unix_ms, entry.lru))
            .map(|(key, entry)| ObservedCost {
                phase: key.phase,
                summary: entry.summary,
                age_ms: age(entry)
                    .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
                    .map(Metric::Known)
                    .unwrap_or(Metric::Unknown(UnknownReason::Stale)),
            });
        HistoryDiagnostics {
            storage: self.storage,
            classes: self.entries.len(),
            recent_classes: self
                .entries
                .values()
                .filter(|entry| age(entry).is_some_and(|elapsed| elapsed <= RECENT_LIFETIME))
                .count(),
            latest,
        }
    }

    /// Predictions are exact-key priors, not activation/compatibility evidence.
    pub fn prior(&mut self, key: HistoryKey, unix_ms: u64) -> Option<CostSummary> {
        let entry = self.entries.get_mut(&key)?;
        if unix_ms.checked_sub(entry.summary.last_unix_ms)? > PRIOR_LIFETIME_MS {
            return None;
        }
        self.clock = self.clock.checked_add(1)?;
        entry.lru = self.clock;
        Some(entry.summary)
    }

    /// Recent same-process service cost is still advisory. Persisted summaries
    /// deliberately have no monotonic stamp, so restart NEVER revives expansion.
    pub fn recent(&self, key: HistoryKey, now: Instant) -> Option<CostSummary> {
        let entry = self.entries.get(&key)?;
        (now.checked_duration_since(entry.last_monotonic?)? <= RECENT_LIFETIME)
            .then_some(entry.summary)
    }

    pub fn observe(&mut self, key: HistoryKey, sample: CostSample) -> bool {
        if !valid_key(key) || sample.completed_units == 0 || sample.elapsed_ns == 0 {
            return false;
        }
        if self.entries.get(&key).is_some_and(|entry| {
            sample.unix_ms < entry.summary.last_unix_ms
                || entry.last_monotonic.is_some_and(|last| sample.at <= last)
        }) {
            return false;
        }
        let Some(cpu) = sample
            .cpu_ns
            .checked_add(sample.completed_units - 1)
            .map(|sum| sum / sample.completed_units)
        else {
            return false;
        };
        let Some(elapsed) = sample
            .elapsed_ns
            .checked_add(sample.completed_units - 1)
            .map(|sum| sum / sample.completed_units)
        else {
            return false;
        };
        let Some(clock) = self.clock.checked_add(1) else {
            return false;
        };
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_CLASSES {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.lru)
                .map(|(key, _)| *key);
            if let Some(victim) = victim {
                self.entries.remove(&victim);
            }
        }
        self.clock = clock;
        let entry = self.entries.entry(key).or_insert(Entry {
            summary: CostSummary {
                samples: 0,
                max_cpu_ns_per_unit: 0,
                max_elapsed_ns_per_unit: 0,
                max_observed_resident_bytes: 0,
                last_unix_ms: sample.unix_ms,
            },
            last_monotonic: None,
            lru: clock,
        });
        entry.summary.samples = entry.summary.samples.saturating_add(1);
        entry.summary.max_cpu_ns_per_unit = entry.summary.max_cpu_ns_per_unit.max(cpu);
        entry.summary.max_elapsed_ns_per_unit = entry.summary.max_elapsed_ns_per_unit.max(elapsed);
        entry.summary.max_observed_resident_bytes = entry
            .summary
            .max_observed_resident_bytes
            .max(sample.observed_resident_bytes);
        entry.summary.last_unix_ms = sample.unix_ms;
        entry.last_monotonic = Some(sample.at);
        entry.lru = clock;
        true
    }

    /// Atomic replacement of bounded summaries only; never stores claims, trial
    /// acceptance, cleanup receipts, raw samples or high-frequency Raft writes.
    pub fn save(&mut self, unix_ms: u64) -> io::Result<bool> {
        let Some(persistence) = &self.persistence else {
            return Ok(false);
        };
        self.entries.retain(|_, entry| {
            unix_ms
                .checked_sub(entry.summary.last_unix_ms)
                .is_some_and(|age| age <= PRIOR_LIFETIME_MS)
        });
        let mut ordered = self.entries.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(_, entry)| entry.lru);
        let image = Image {
            schema: 1,
            node_instance: self.node_instance,
            entries: ordered
                .into_iter()
                .map(|(key, entry)| (*key, entry.summary))
                .collect(),
        };
        let bytes = serde_json::to_vec(&image).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other("resource history size bound exceeded"));
        }
        let mut staged = tempfile::NamedTempFile::new_in(&persistence.directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged
            .persist(persistence.directory.join("summaries.json"))
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(&persistence.directory)?.sync_all()?;
        Ok(true)
    }
}

fn valid_key(key: HistoryKey) -> bool {
    [
        key.runtime_epoch,
        key.workload,
        key.recipe,
        key.concurrent_mix,
    ]
    .into_iter()
    .all(|digest| digest != [0; 32])
}

fn open_persistence(directory: &Path) -> io::Result<Option<Persistence>> {
    std::fs::create_dir_all(directory)?;
    if !std::fs::symlink_metadata(directory)?.file_type().is_dir() {
        return Err(io::Error::other(
            "resource history requires a real directory",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock = options.open(directory.join("owner.lock"))?;
    if !lock.metadata()?.is_file() {
        return Err(io::Error::other(
            "resource history lock is not a regular file",
        ));
    }
    match lock.try_lock() {
        Ok(()) => Ok(Some(Persistence {
            directory: directory.to_owned(),
            _lock: lock,
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn read_image(path: &Path) -> io::Result<Option<Image>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_BYTES => {}
        _ => return Err(io::Error::other("unrecognized resource history file")),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("nonregular resource history"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other("oversized resource history"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(index: u64) -> HistoryKey {
        let mut workload = [1; 32];
        workload[..8].copy_from_slice(&index.to_be_bytes());
        HistoryKey {
            runtime_epoch: [1; 32],
            workload,
            recipe: [2; 32],
            concurrent_mix: [3; 32],
            phase: CostPhase::Steady,
        }
    }
    fn sample(at: Instant, unix_ms: u64) -> CostSample {
        CostSample {
            at,
            unix_ms,
            cpu_ns: 100,
            completed_units: 3,
            elapsed_ns: 200,
            observed_resident_bytes: 1024,
        }
    }
    #[test]
    fn exclusive_owner_conflict_is_empty_volatile_and_restart_restores_only_priors() {
        let directory = tempfile::tempdir().expect("durable test TMPDIR");
        let node = uuid::Uuid::new_v4();
        let now = Instant::now();
        let mut first = ResourceHistory::open(directory.path(), node, 1000);
        assert_eq!(first.storage(), HistoryStorage::Persistent);
        assert!(first.observe(key(1), sample(now, 1000)));
        let live = first.diagnostics(now);
        assert_eq!((live.classes, live.recent_classes), (1, 1));
        assert!(matches!(
            live.latest.unwrap().age_ms,
            crate::process::observation::Metric::Known(0)
        ));
        assert_eq!(
            first
                .diagnostics(now + Duration::from_secs(16))
                .recent_classes,
            0
        );
        assert!(first.save(1000).expect("atomic save"));
        let mut second = ResourceHistory::open(directory.path(), node, 1000);
        assert_eq!(second.storage(), HistoryStorage::LockConflictVolatile);
        assert!(second.is_empty());
        assert!(second.observe(key(2), sample(now, 1000)));
        assert!(!second.save(1000).expect("cannot overwrite first owner"));
        drop(first);
        let mut restarted = ResourceHistory::open(directory.path(), node, 1001);
        assert!(restarted.prior(key(1), 1001).is_some());
        assert!(restarted.recent(key(1), now).is_none());
        let prior = restarted.diagnostics(now);
        assert_eq!((prior.classes, prior.recent_classes), (1, 0));
        assert!(matches!(
            prior.latest.unwrap().age_ms,
            crate::process::observation::Metric::Unknown(
                crate::process::observation::UnknownReason::Stale
            )
        ));
        assert!(restarted.prior(key(2), 1001).is_none());
    }
    #[test]
    fn identities_phases_lru_and_evidence_ages_are_bounded_without_concurrency_inference() {
        let directory = tempfile::tempdir().expect("durable test TMPDIR");
        let now = Instant::now();
        let mut history = ResourceHistory::open(directory.path(), uuid::Uuid::new_v4(), 1000);
        assert!(history.observe(key(0), sample(now, 1000)));
        assert!(
            !history.observe(key(0), sample(now, 1000)),
            "duplicate ignored"
        );
        assert_eq!(
            history
                .recent(key(0), now)
                .expect("current cost")
                .max_cpu_ns_per_unit,
            34
        );
        for changed in [
            HistoryKey {
                runtime_epoch: [4; 32],
                ..key(0)
            },
            HistoryKey {
                phase: CostPhase::Startup,
                ..key(0)
            },
            HistoryKey {
                concurrent_mix: [5; 32],
                ..key(0)
            },
        ] {
            assert!(history.prior(changed, 1000).is_none());
        }
        for index in 1..=1024 {
            assert!(history.observe(key(index), sample(now, 1000)));
        }
        assert_eq!(history.len(), MAX_CLASSES);
        assert!(history.prior(key(0), 1000).is_none());
        assert!(history
            .recent(key(1024), now + Duration::from_secs(16))
            .is_none());
        assert!(history
            .prior(key(1024), 1000 + PRIOR_LIFETIME_MS + 1)
            .is_none());
        assert!(!history.observe(
            key(5),
            CostSample {
                completed_units: 0,
                ..sample(now, 1000)
            }
        ));
    }
    #[test]
    fn corrupt_or_future_history_is_preserved_and_never_overwritten() {
        let directory = tempfile::tempdir().expect("durable test TMPDIR");
        let node = uuid::Uuid::new_v4();
        let first = ResourceHistory::open(directory.path(), node, 1000);
        drop(first);
        let path = directory
            .path()
            .join("resource-history")
            .join(node.simple().to_string())
            .join("summaries.json");
        for bytes in [b"not JSON".as_slice(), b"{\"schema\":2}".as_slice()] {
            std::fs::write(&path, bytes).expect("corrupt input");
            let mut history = ResourceHistory::open(directory.path(), node, 1000);
            assert_eq!(history.storage(), HistoryStorage::UnrecognizedVolatile);
            assert!(history.is_empty());
            assert!(!history.save(1000).expect("refuse unknown format"));
            assert_eq!(std::fs::read(&path).expect("preserved bytes"), bytes);
        }
    }
}
