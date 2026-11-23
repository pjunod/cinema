//! Bounded observational DB AppendEntries timing. Never used for authority.
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) const CAPACITY: usize = 64;
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
pub(crate) fn new_source_epoch() -> u64 {
    NEXT_SOURCE.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppendTimingStage {
    RpcAttempt,
    ReceiveEntry,
    ReceiveCompletion,
}
impl AppendTimingStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::RpcAttempt => "rpc_attempt",
            Self::ReceiveEntry => "receive_entry",
            Self::ReceiveCompletion => "receive_completion",
        }
    }
}

/// A process/source/term/endpoint-scoped summary. Gaps include data appends;
/// they are not physical socket writes, network delivery or RTT samples.
#[derive(Debug, Clone)]
pub struct AppendTimingObservation {
    pub local_node: u64,
    pub remote_node: u64,
    pub stage: AppendTimingStage,
    pub source_epoch: u64,
    pub term: u64,
    pub endpoint_fingerprint: u64,
    pub samples: u64,
    pub heartbeats: u64,
    pub errors: u64,
    pub gap_samples: u64,
    pub gap_sum_seconds: f64,
    pub gap_max_seconds: f64,
    pub elapsed_sum_seconds: f64,
    pub elapsed_max_seconds: f64,
    pub since_last_seconds: f64,
}
#[derive(Debug, Clone)]
pub struct AppendTimingSnapshot {
    pub process_epoch: u64,
    pub evictions: u64,
    pub rows: Vec<AppendTimingObservation>,
}
struct Row {
    value: AppendTimingObservation,
    last: Instant,
}
#[derive(Default)]
struct Registry {
    rows: BTreeMap<(u64, u64, AppendTimingStage), Row>,
    evictions: u64,
}
impl Registry {
    fn record(
        &mut self,
        mut value: AppendTimingObservation,
        now: Instant,
        elapsed: Duration,
        empty: bool,
        failed: bool,
    ) {
        let key = (value.local_node, value.remote_node, value.stage);
        if let Some(old) = self.rows.get(&key) {
            if value.source_epoch < old.value.source_epoch
                || (value.source_epoch == old.value.source_epoch && value.term < old.value.term)
            {
                return;
            }
            if value.source_epoch == old.value.source_epoch
                && value.term == old.value.term
                && value.endpoint_fingerprint == old.value.endpoint_fingerprint
            {
                let Some(gap) = now.checked_duration_since(old.last) else {
                    return;
                };
                value = old.value.clone();
                value.gap_samples = value.gap_samples.saturating_add(1);
                value.gap_sum_seconds += gap.as_secs_f64();
                value.gap_max_seconds = value.gap_max_seconds.max(gap.as_secs_f64());
            }
        } else if self.rows.len() >= CAPACITY
            && let Some(key) = self
                .rows
                .iter()
                .min_by_key(|(_, row)| row.last)
                .map(|(key, _)| *key)
        {
            self.rows.remove(&key);
            self.evictions = self.evictions.saturating_add(1);
        }
        value.samples = value.samples.saturating_add(1);
        value.heartbeats = value.heartbeats.saturating_add(u64::from(empty));
        value.errors = value.errors.saturating_add(u64::from(failed));
        value.elapsed_sum_seconds += elapsed.as_secs_f64();
        value.elapsed_max_seconds = value.elapsed_max_seconds.max(elapsed.as_secs_f64());
        self.rows.insert(key, Row { value, last: now });
    }
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}
fn observation(
    local_node: u64,
    remote_node: u64,
    source_epoch: u64,
    term: u64,
    endpoint: &str,
    stage: AppendTimingStage,
) -> AppendTimingObservation {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    endpoint.hash(&mut hash);
    AppendTimingObservation {
        local_node,
        remote_node,
        stage,
        source_epoch,
        term,
        endpoint_fingerprint: hash.finish(),
        samples: 0,
        heartbeats: 0,
        errors: 0,
        gap_samples: 0,
        gap_sum_seconds: 0.,
        gap_max_seconds: 0.,
        elapsed_sum_seconds: 0.,
        elapsed_max_seconds: 0.,
        since_last_seconds: 0.,
    }
}
pub(crate) struct AppendTimingStamp {
    value: AppendTimingObservation,
    started: Instant,
    empty: bool,
}
impl AppendTimingStamp {
    pub(crate) fn complete(self, failed: bool) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.started);
        let mut value = self.value;
        value.stage = AppendTimingStage::ReceiveCompletion;
        if let Ok(mut registry) = registry().lock() {
            registry.record(value, now, elapsed, self.empty, failed);
        }
    }
}
pub(crate) fn record_append(
    local: u64,
    remote: u64,
    source: u64,
    term: u64,
    endpoint: &str,
    stage: AppendTimingStage,
    empty: bool,
) -> AppendTimingStamp {
    let started = Instant::now();
    let value = observation(local, remote, source, term, endpoint, stage);
    if let Ok(mut registry) = registry().lock() {
        registry.record(value.clone(), started, Duration::ZERO, empty, false);
    }
    AppendTimingStamp {
        value,
        started,
        empty,
    }
}
/// Flat passive snapshot, bounded to64rows. Poisoned observation state yields
/// no rows, never a failure in Raft or a change of admission policy.
pub fn append_timing_observations() -> AppendTimingSnapshot {
    static PROCESS_EPOCH: OnceLock<u64> = OnceLock::new();
    let process_epoch = *PROCESS_EPOCH.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    });
    let now = Instant::now();
    let Ok(registry) = registry().lock() else {
        return AppendTimingSnapshot {
            process_epoch,
            evictions: 0,
            rows: Vec::new(),
        };
    };
    let rows = registry
        .rows
        .values()
        .map(|row| {
            let mut value = row.value.clone();
            value.since_last_seconds = now.saturating_duration_since(row.last).as_secs_f64();
            value
        })
        .collect();
    AppendTimingSnapshot {
        process_epoch,
        evictions: registry.evictions,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(source: u64, term: u64, endpoint: &str) -> AppendTimingObservation {
        observation(1, 2, source, term, endpoint, AppendTimingStage::RpcAttempt)
    }
    #[test]
    fn gaps_include_heartbeats_and_data_and_reset_on_identity_changes() {
        let mut r = Registry::default();
        let now = Instant::now();
        r.record(value(1, 1, "a"), now, Duration::ZERO, true, false);
        r.record(
            value(1, 1, "a"),
            now + Duration::from_secs(2),
            Duration::ZERO,
            false,
            false,
        );
        let row = &r.rows.values().next().unwrap().value;
        assert_eq!((row.samples, row.heartbeats, row.gap_samples), (2, 1, 1));
        assert_eq!(row.gap_max_seconds, 2.);
        for (source, term, endpoint) in [(1, 2, "a"), (1, 2, "b"), (2, 2, "b")] {
            r.record(
                value(source, term, endpoint),
                now + Duration::from_secs(3),
                Duration::ZERO,
                false,
                false,
            );
            assert_eq!(r.rows.values().next().unwrap().value.gap_samples, 0);
        }
        r.record(
            value(1, 99, "b"),
            now + Duration::from_secs(99),
            Duration::ZERO,
            false,
            false,
        );
        assert_eq!(r.rows.values().next().unwrap().value.source_epoch, 2);
        r.record(
            value(2, 1, "b"),
            now + Duration::from_secs(99),
            Duration::ZERO,
            false,
            false,
        );
        assert_eq!(r.rows.values().next().unwrap().value.term, 2);
    }
    #[test]
    fn registry_and_completion_latency_are_bounded_observations() {
        let mut r = Registry::default();
        let now = Instant::now();
        for remote in 0..100 {
            let mut v = value(1, 1, "a");
            v.remote_node = remote;
            v.stage = AppendTimingStage::ReceiveCompletion;
            r.record(
                v,
                now + Duration::from_millis(remote),
                Duration::from_millis(7),
                false,
                true,
            );
        }
        assert_eq!(r.rows.len(), CAPACITY);
        assert_eq!(r.evictions, 36);
        let row = &r.rows.values().next().unwrap().value;
        assert_eq!(row.errors, 1);
        assert_eq!(row.elapsed_max_seconds, 0.007);
    }
}
