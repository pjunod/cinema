//! Bounded, node-local observations for item detail. Playback still decides
//! whether a source can open at the actual open site.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use tokio::sync::{watch, Semaphore};
use tokio::time::Instant;

// Repeat detail opens should not wake a NAS for every request.
const TTL: Duration = Duration::from_secs(15);
// A two-minute-old presence fact cannot describe a recently unmounted share.
const MAX_AGE: Duration = Duration::from_secs(120);
// Blocked metadata syscalls cannot be cancelled; cap their worker pressure.
const PROBES: usize = 8;
// Count paths waiting for a permit as well as paths already probing.
const MAX_INFLIGHT: usize = 1_024;
// Covers the largest library on the fleet in single-digit megabytes.
const ENTRIES: usize = 16_384;
// One deadline for the page, not one deadline per audiobook part.
const REQUEST_BUDGET: Duration = Duration::from_millis(250);
// A wedged export stays visible in System logs without flooding them.
const WARN_AFTER: Duration = Duration::from_secs(5);
const WARN_AGAIN: Duration = Duration::from_secs(3_600);
const PROBE_BUCKETS_US: [u64; 7] = [
    1_000,
    10_000,
    50_000,
    250_000,
    1_000_000,
    5_000_000,
    u64::MAX,
];

type ProbeFuture = Pin<Box<dyn Future<Output = bool> + Send>>;
type Probe = dyn Fn(PathBuf) -> ProbeFuture + Send + Sync;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available,
    Unavailable,
    Unknown,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub state: Availability,
    pub observed_at_ms: Option<i64>,
}

impl Observation {
    const UNKNOWN: Self = Self {
        state: Availability::Unknown,
        observed_at_ms: None,
    };

    /// A client with only the legacy boolean may try playback; the real open
    /// remains the authority when no bounded observation is available.
    pub fn available(self) -> bool {
        self.state != Availability::Unavailable
    }

    pub fn missing_path(self, admin: bool, path: &Path) -> Option<String> {
        (admin && self.state == Availability::Unavailable)
            .then(|| path.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Copy)]
struct Cached {
    observation: Observation,
    at: Instant,
    sequence: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<PathBuf, Cached>,
    inflight: HashMap<PathBuf, watch::Receiver<Option<Observation>>>,
    sequence: u64,
}

struct Limits {
    probes: usize,
    inflight: usize,
    entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            probes: PROBES,
            inflight: MAX_INFLIGHT,
            entries: ENTRIES,
        }
    }
}

static ANSWERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
static PROBE_COUNTS: [AtomicU64; PROBE_BUCKETS_US.len()] =
    [const { AtomicU64::new(0) }; PROBE_BUCKETS_US.len()];
static PROBE_SUM_US: AtomicU64 = AtomicU64::new(0);

pub struct AvailabilityCache {
    inner: Mutex<Inner>,
    permits: Arc<Semaphore>,
    limits: Limits,
    probe: Arc<Probe>,
}

impl AvailabilityCache {
    pub fn new() -> Arc<Self> {
        Self::with_probe(
            Limits::default(),
            Arc::new(|path| Box::pin(async move { tokio::fs::metadata(path).await.is_ok() })),
        )
    }

    fn with_probe(limits: Limits, probe: Arc<Probe>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            permits: Arc::new(Semaphore::new(limits.probes)),
            limits,
            probe,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_probe<F, Fut>(probe: F) -> Arc<Self>
    where
        F: Fn(PathBuf) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        Self::with_probe(
            Limits::default(),
            Arc::new(move |path| Box::pin(probe(path))),
        )
    }

    /// Start all uncached stats before waiting. A stale but still honest
    /// observation answers immediately while its refresh continues.
    pub async fn observe_many(self: &Arc<Self>, paths: &[PathBuf]) -> Vec<Observation> {
        let mut answers = Vec::with_capacity(paths.len());
        let mut pending = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let (answer, receiver) = self.lookup_or_schedule(path);
            answers.push(answer);
            if let Some(receiver) = receiver {
                pending.push((index, receiver));
            }
        }

        if !pending.is_empty() {
            // One page budget, not 250 ms multiplied by audiobook part count.
            let _ = tokio::time::timeout(
                REQUEST_BUDGET,
                join_all(pending.iter_mut().map(|(_, receiver)| async move {
                    if receiver.borrow().is_none() {
                        let _ = receiver.changed().await;
                    }
                })),
            )
            .await;
            for (index, receiver) in pending {
                if let Some(observation) = *receiver.borrow() {
                    answers[index] = observation;
                }
            }
        }

        for answer in &answers {
            let slot = match answer.state {
                Availability::Available => 0,
                Availability::Unavailable => 1,
                Availability::Unknown => 2,
            };
            ANSWERS[slot].fetch_add(1, Ordering::Relaxed);
        }
        answers
    }

    fn lookup_or_schedule(
        self: &Arc<Self>,
        path: &Path,
    ) -> (Observation, Option<watch::Receiver<Option<Observation>>>) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("availability cache mutex");
        let cached = inner.entries.get(path).copied();
        if let Some(entry) = cached {
            if now.duration_since(entry.at) < TTL {
                return (entry.observation, None);
            }
        }

        let answer = match cached {
            Some(entry) if now.duration_since(entry.at) < MAX_AGE => entry.observation,
            // Keep the timestamp of the stale fact so a client can see why
            // this is Unknown; never return its state as current evidence.
            Some(entry) => Observation {
                state: Availability::Unknown,
                observed_at_ms: entry.observation.observed_at_ms,
            },
            None => Observation::UNKNOWN,
        };
        if let Some(receiver) = inner.inflight.get(path) {
            return (
                answer,
                (answer.state == Availability::Unknown).then(|| receiver.clone()),
            );
        }
        if inner.inflight.len() >= self.limits.inflight {
            // Past TTL the old state is only useful when a refresh is in
            // progress. At capacity no refresh can start, so even a cached
            // Unavailable must not keep refusing a newly remounted file.
            return (
                Observation {
                    state: Availability::Unknown,
                    observed_at_ms: answer.observed_at_ms,
                },
                None,
            );
        }

        let path = path.to_path_buf();
        let (sender, receiver) = watch::channel(None);
        inner.inflight.insert(path.clone(), receiver.clone());
        INFLIGHT.fetch_add(1, Ordering::Relaxed);
        drop(inner);
        let cache = Arc::clone(self);
        tokio::spawn(async move { cache.run_probe(path, sender).await });
        (
            answer,
            (answer.state == Availability::Unknown).then_some(receiver),
        )
    }

    async fn run_probe(self: Arc<Self>, path: PathBuf, sender: watch::Sender<Option<Observation>>) {
        // Waiting is bounded by the in-flight map. try_acquire would turn
        // temporary saturation into an unexplained Unknown answer.
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore open");
        let started = Instant::now();
        let probe = (self.probe)(path.clone());
        tokio::pin!(probe);
        let mut warning = Box::pin(tokio::time::sleep(WARN_AFTER));
        let present = loop {
            tokio::select! {
                result = &mut probe => break result,
                () = &mut warning => {
                    // The blocked fs operation keeps its worker until the
                    // kernel returns; this warning has no path label.
                    tracing::warn!("storage probe exceeded 5s for a mount");
                    warning.as_mut().reset(Instant::now() + WARN_AGAIN);
                }
            }
        };
        let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        PROBE_SUM_US.fetch_add(elapsed_us, Ordering::Relaxed);
        for (bucket, count) in PROBE_BUCKETS_US.iter().zip(&PROBE_COUNTS) {
            if elapsed_us <= *bucket {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        let observation = Observation {
            state: if present {
                Availability::Available
            } else {
                Availability::Unavailable
            },
            observed_at_ms: Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64,
            ),
        };
        {
            let mut inner = self.inner.lock().expect("availability cache mutex");
            inner.sequence = inner.sequence.wrapping_add(1);
            let sequence = inner.sequence;
            inner.entries.insert(
                path.clone(),
                Cached {
                    observation,
                    at: Instant::now(),
                    sequence,
                },
            );
            // Age first; insertion order breaks equal-age ties. Keeping only
            // the oldest eviction candidate makes the bound exact.
            while inner.entries.len() > self.limits.entries {
                if let Some(oldest) = inner
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| (entry.at, entry.sequence))
                    .map(|(path, _)| path.clone())
                {
                    inner.entries.remove(&oldest);
                }
            }
            inner.inflight.remove(&path);
        }
        INFLIGHT.fetch_sub(1, Ordering::Relaxed);
        let _ = sender.send(Some(observation));
    }
}

/// Fixed labels and atomics only: a metrics scrape never touches a Store or
/// tries to lock the observation map.
pub fn prometheus() -> String {
    let mut out = String::from(
        "# HELP plurx_storage_availability_total Detail-file availability answers.\n\
         # TYPE plurx_storage_availability_total counter\n",
    );
    for (state, count) in ["available", "unavailable", "unknown"].iter().zip(&ANSWERS) {
        out.push_str(&format!(
            "plurx_storage_availability_total{{result=\"{state}\"}} {}\n",
            count.load(Ordering::Relaxed)
        ));
    }
    out.push_str(&format!(
        "# HELP plurx_storage_availability_probes_inflight Distinct paths awaiting a storage probe.\n\
         # TYPE plurx_storage_availability_probes_inflight gauge\n\
         plurx_storage_availability_probes_inflight {}\n\
         # HELP plurx_storage_availability_probe_seconds Completed storage probe duration.\n\
         # TYPE plurx_storage_availability_probe_seconds histogram\n",
        INFLIGHT.load(Ordering::Relaxed)
    ));
    for (bucket, count) in PROBE_BUCKETS_US.iter().zip(&PROBE_COUNTS) {
        let label = if *bucket == u64::MAX {
            "+Inf".to_owned()
        } else {
            format!("{:.3}", *bucket as f64 / 1_000_000.0)
        };
        out.push_str(&format!(
            "plurx_storage_availability_probe_seconds_bucket{{le=\"{label}\"}} {}\n",
            count.load(Ordering::Relaxed)
        ));
    }
    out.push_str(&format!(
        "plurx_storage_availability_probe_seconds_sum {:.6}\n\
         plurx_storage_availability_probe_seconds_count {}\n",
        PROBE_SUM_US.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        PROBE_COUNTS[PROBE_COUNTS.len() - 1].load(Ordering::Relaxed)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn fake(probe: Arc<Probe>) -> Arc<AvailabilityCache> {
        AvailabilityCache::with_probe(
            Limits {
                probes: 1,
                inflight: 2,
                entries: 2,
            },
            probe,
        )
    }

    async fn ready(cache: &Arc<AvailabilityCache>, path: &str) -> Observation {
        cache.observe_many(&[PathBuf::from(path)]).await[0]
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_available_answers_without_a_stat() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let cache = fake(Arc::new(move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { true })
        }));
        assert_eq!(ready(&cache, "one").await.state, Availability::Available);
        assert_eq!(ready(&cache, "one").await.state, Availability::Available);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_observation_past_max_age_answers_unknown() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let gate = Arc::new(Semaphore::new(0));
        let held = Arc::clone(&gate);
        let cache = fake(Arc::new(move |_| {
            let n = seen.fetch_add(1, Ordering::Relaxed);
            let held = Arc::clone(&held);
            Box::pin(async move {
                if n > 0 {
                    let _permit = held.acquire().await.expect("gate open");
                }
                true
            })
        }));
        let first = ready(&cache, "one").await;
        tokio::time::advance(MAX_AGE + Duration::from_secs(1)).await;
        let stale = ready(&cache, "one").await;
        assert_eq!(stale.state, Availability::Unknown);
        assert_eq!(stale.observed_at_ms, first.observed_at_ms);
        gate.add_permits(1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_caller_leaves_one_probe_and_no_duplicate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let gate = Arc::new(Semaphore::new(0));
        let held = Arc::clone(&gate);
        let cache = fake(Arc::new(move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
            let held = Arc::clone(&held);
            Box::pin(async move {
                let _permit = held.acquire().await.expect("gate open");
                true
            })
        }));
        assert_eq!(ready(&cache, "one").await.state, Availability::Unknown);
        assert_eq!(ready(&cache, "one").await.state, Availability::Unknown);
        assert_eq!(cache.inner.lock().expect("cache mutex").inflight.len(), 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        gate.add_permits(1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_inflight_cap_answers_unknown_instead_of_growing() {
        let gate = Arc::new(Semaphore::new(0));
        let held = Arc::clone(&gate);
        let cache = AvailabilityCache::with_probe(
            Limits {
                probes: 1,
                inflight: 1,
                entries: 2,
            },
            Arc::new(move |_| {
                let held = Arc::clone(&held);
                Box::pin(async move {
                    let _permit = held.acquire().await.expect("gate open");
                    true
                })
            }),
        );
        let result = cache
            .observe_many(&[PathBuf::from("one"), PathBuf::from("two")])
            .await;
        assert!(result
            .iter()
            .all(|entry| entry.state == Availability::Unknown));
        assert_eq!(cache.inner.lock().expect("cache mutex").inflight.len(), 1);
        gate.add_permits(1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_saturated_inflight_map_demotes_stale_unavailable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let gate = Arc::new(Semaphore::new(0));
        let held = Arc::clone(&gate);
        let cache = AvailabilityCache::with_probe(
            Limits {
                probes: 1,
                inflight: 1,
                entries: 2,
            },
            Arc::new(move |path| {
                seen.fetch_add(1, Ordering::Relaxed);
                let held = Arc::clone(&held);
                Box::pin(async move {
                    if path == Path::new("blocked") {
                        let _permit = held.acquire().await.expect("gate open");
                    }
                    false
                })
            }),
        );
        let missing = ready(&cache, "missing").await;
        assert_eq!(missing.state, Availability::Unavailable);
        tokio::time::advance(TTL + Duration::from_secs(1)).await;
        assert_eq!(ready(&cache, "blocked").await.state, Availability::Unknown);
        assert_eq!(cache.inner.lock().expect("cache mutex").inflight.len(), 1);

        let saturated = ready(&cache, "missing").await;
        assert_eq!(saturated.state, Availability::Unknown);
        assert_eq!(saturated.observed_at_ms, missing.observed_at_ms);
        assert!(saturated.available());
        assert_eq!(saturated.missing_path(true, Path::new("missing")), None);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        gate.add_permits(1);
    }

    #[test]
    fn unknown_does_not_set_available_false() {
        assert!(Observation::UNKNOWN.available());
        assert!(!Observation {
            state: Availability::Unavailable,
            observed_at_ms: Some(1),
        }
        .available());
    }

    #[test]
    fn missing_path_is_set_only_for_unavailable() {
        let path = Path::new("/media/missing.mp4");
        assert_eq!(Observation::UNKNOWN.missing_path(true, path), None);
        let missing = Observation {
            state: Availability::Unavailable,
            observed_at_ms: Some(1),
        };
        assert_eq!(missing.missing_path(false, path), None);
        assert_eq!(
            missing.missing_path(true, path),
            Some("/media/missing.mp4".to_owned())
        );
    }

    #[test]
    fn the_playback_start_path_never_reads_the_cache() {
        // The real GET and POST routes share `decision`; this catches a
        // future edit that diverts either through the detail observation.
        let routes = include_str!("http/mod.rs");
        let decision_route = routes
            .split_once("\"/files/{id}/decision\",")
            .expect("playback decision route")
            .1;
        assert!(decision_route.contains("get(stream::decision)"));
        assert!(decision_route.contains(".post(stream::decision_post)"));

        let source = include_str!("http/stream.rs");
        let start = source
            .split_once("pub async fn decision(")
            .expect("playback decision handler")
            .1;
        assert!(start.contains("state.availability.is_present(id, &file.path).await"));
        assert!(!source.contains("detail_availability"));
    }
}
