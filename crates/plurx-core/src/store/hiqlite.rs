//! The complete replicated durable-store backend.
//!
//! The trait implementations are split across this module and the sibling
//! catalogue, media, and durable modules. SQLite remains the daemon's selected
//! [`Store`](super::Store) until M2 imports existing state and activates this
//! backend; backend completeness alone is not permission to skip that gate.

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::path::Path;
#[cfg(feature = "cluster-read-cost-validation")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::{Client, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::replicated::ReplicatedSql;
use super::telemetry::NodeLocalTelemetry;
use super::{
    keys, ApiKeyStore, ArtworkRepairFence, MetricsStore, NetworkPriorStore, PlaybackTelemetryStore,
    PrometheusStoreSnapshot, SettingsStore, UserStore,
};
use crate::domain::{
    ApiKey, NetworkPrior, NetworkPriorObservation, OfflinePackageStats, PlaybackEvent,
    PlaybackEventQuery, User,
};
use crate::error::StoreError;

// v6 adds revision-bound ebook reading state; v7 adds first-class book facts;
// v8 adds monotone cluster-work leases; v9 adds the distributed whole-title
// speculative-transcode queue; v10 adds live media-session routing; v11 adds
// storage-keyed shared-cache generations and reader pins; v12 adds the small
// replicated catalog and fenced queue for content-addressed fragment indexes.
// Every additive step
// is applied through Raft before the daemon opens the store. v5 remains a
// supported direct-upgrade source so an offline node is
// not forced to install every intermediate Cinema release; older or future
// schemas still fail closed.
pub const AUTH_SCHEMA_VERSION: i64 = 12;
/// Oldest schema this binary can advance through the complete migration chain.
pub const AUTH_SCHEMA_MIGRATION_SOURCE: i64 = 5;
const READING_SCHEMA_VERSION: i64 = 6;
const BOOK_SCHEMA_MIGRATION_SOURCE: i64 = READING_SCHEMA_VERSION;
const LEASE_SCHEMA_MIGRATION_SOURCE: i64 = 7;
const PRETRANSCODE_SCHEMA_MIGRATION_SOURCE: i64 = 8;
const MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE: i64 = 9;
const SHARED_CACHE_SCHEMA_MIGRATION_SOURCE: i64 = 10;
const FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE: i64 = 11;
// Session routing and shared-cache identity are additive durable state and use
// the existing Hiqlite transport contract. Protocol 4 stays supported so a
// healthy v9/v10 cluster can authorize the daemon that advances its schema.
//
// P6 turns the single protocol number into a supported *range*. This binary
// implements every protocol in `AUTH_PROTOCOL_MIN..=AUTH_PROTOCOL_MAX`, while
// the cluster records the range its features actually depend on in
// `cluster_meta.protocol_min..=protocol_max`. Installing this binary therefore
// changes nothing by itself: the range widens on the binary side only, and
// only an explicit activation narrows a cluster onto protocol 5.
/// Oldest replicated protocol this binary can still participate in.
pub const AUTH_PROTOCOL_MIN: i64 = 4;
/// Newest replicated protocol this binary implements. Protocol 5 is the
/// non-voting learner admission protocol; it stays inert until activated.
pub const AUTH_PROTOCOL_MAX: i64 = 5;
/// The single protocol scalar this binary still puts on the v1 join wire.
///
/// A peer that predates the range compares this field for exact equality, so
/// it must keep naming the oldest protocol we support; otherwise every
/// mixed-version join breaks before either side reads a range field.
pub const AUTH_PROTOCOL_VERSION: i64 = AUTH_PROTOCOL_MIN;
/// The protocol under which a cluster can admit a non-voting learner at all.
///
/// Named separately from [`AUTH_PROTOCOL_MAX`] because the two mean different
/// things: `AUTH_PROTOCOL_MAX` is "the newest thing this build implements" and
/// moves with every future protocol, while this is "the protocol that carries
/// learner admission" and must not.
pub const AUTH_LEARNER_PROTOCOL: i64 = 5;

const STORE_TIMEOUT: Duration = Duration::from_secs(3);
const AUTHORITY_READ_RETRY_DELAY: Duration = Duration::from_millis(100);
const AUTHORITY_READ_MAX_ATTEMPTS: usize = 2;
const IDEMPOTENT_WRITE_RETRY_DELAY: Duration = Duration::from_millis(100);
const IDEMPOTENT_WRITE_MAX_ATTEMPTS: usize = 3;
const REPLICATED_STORE_TIMEOUT: &str = "replicated store operation timed out";

const AUTH_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cluster_meta (
    singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version   INTEGER NOT NULL,
    protocol_min     INTEGER NOT NULL,
    protocol_max     INTEGER NOT NULL,
    migrated_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS tokens (
    token_hash   TEXT PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device       TEXT,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS api_keys (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    scopes       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    disabled     INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS job_leases (
    resource       TEXT PRIMARY KEY,
    owner_node_id  TEXT NOT NULL,
    fence          INTEGER NOT NULL CHECK (fence > 0),
    revision       INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT;
"#;

/// What one binary can participate in: exactly one schema version, and every
/// protocol in an inclusive range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterCompatibility {
    pub schema_version: i64,
    pub protocol_min: i64,
    pub protocol_max: i64,
}

/// Deterministic client-call accounting for clustered-read regression gates.
///
/// These are attempted [`TimedClient`] API calls, not network RTTs, quorum
/// messages, or a claim that a non-consistent query executed locally. The
/// validation feature is absent from production builds.
#[cfg(feature = "cluster-read-cost-validation")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiqliteOperationCounts {
    pub consistent_query_calls: u64,
    pub non_consistent_query_calls: u64,
    pub write_calls: u64,
}

#[cfg(feature = "cluster-read-cost-validation")]
#[derive(Default)]
struct OperationCounters {
    consistent_query_calls: AtomicU64,
    non_consistent_query_calls: AtomicU64,
    write_calls: AtomicU64,
    fail_next_non_consistent_query: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "cluster-read-cost-validation")]
impl OperationCounters {
    fn reset(&self) {
        self.consistent_query_calls.store(0, Ordering::Relaxed);
        self.non_consistent_query_calls.store(0, Ordering::Relaxed);
        self.write_calls.store(0, Ordering::Relaxed);
        self.fail_next_non_consistent_query
            .store(false, Ordering::Relaxed);
    }

    fn snapshot(&self) -> HiqliteOperationCounts {
        HiqliteOperationCounts {
            consistent_query_calls: self.consistent_query_calls.load(Ordering::Relaxed),
            non_consistent_query_calls: self.non_consistent_query_calls.load(Ordering::Relaxed),
            write_calls: self.write_calls.load(Ordering::Relaxed),
        }
    }
}

impl ClusterCompatibility {
    pub const CURRENT: Self = Self {
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_min: AUTH_PROTOCOL_MIN,
        protocol_max: AUTH_PROTOCOL_MAX,
    };

    /// The P6 compatibility rule, in one place.
    ///
    /// `cluster_min..=cluster_max` is the range of protocols the cluster's
    /// features are *actively using*, so a participant has to implement all of
    /// it — not merely overlap with it. Overlap would let a binary that only
    /// speaks 4 join a cluster whose learners depend on 5.
    #[must_use]
    pub fn covers(&self, cluster_min: i64, cluster_max: i64) -> bool {
        self.protocol_min <= cluster_min && cluster_max <= self.protocol_max
    }

    /// Why this binary cannot participate, written for an operator reading a
    /// log at three in the morning: which side is behind, and what to do.
    #[must_use]
    pub fn protocol_refusal(&self, cluster_min: i64, cluster_max: i64) -> String {
        if cluster_max > self.protocol_max {
            format!(
                "this cluster has activated protocol {cluster_max} (active range \
                 {cluster_min}..={cluster_max}) and this binary is too old for it: it implements \
                 only protocol {}..={}. Install a build that supports protocol {cluster_max} on \
                 this node, or deactivate protocol {cluster_max} from a node that already runs \
                 the newer build.",
                self.protocol_min, self.protocol_max
            )
        } else {
            format!(
                "this cluster still requires protocol {cluster_min} (active range \
                 {cluster_min}..={cluster_max}) and this binary is too new for it: it implements \
                 only protocol {}..={} and has dropped protocol {cluster_min}. Move the cluster \
                 forward from a node that still supports protocol {cluster_min}, or install a \
                 build that still supports it on this node.",
                self.protocol_min, self.protocol_max
            )
        }
    }
}

/// A hiqlite client implementing the complete replicated [`Store`](super::Store).
#[derive(Clone)]
pub struct HiqliteAuthStore {
    client: TimedClient,
    clock: Arc<dyn Clock>,
    telemetry: NodeLocalTelemetry,
    activity_refreshes: Arc<ActivityRefreshGate>,
    cache_touches: Arc<ReplaceableWriteGate<CacheTouchKey>>,
}

/// Cache activity is advisory recency, not ownership or completion. One
/// successful quorum write therefore covers repeated touches of the same
/// location for this bounded interval. Completion, invalidation, and removal
/// never enter this gate and remain synchronous durable mutations.
pub(super) const CACHE_TOUCH_COMMIT_WINDOW: Duration = Duration::from_secs(5);
const REPLACEABLE_WRITE_GATE_MAX_KEYS: usize = 4_096;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum CacheTouchKey {
    Claim {
        recipe_hash: String,
        node_id: String,
    },
    Use {
        recipe_hash: String,
        node_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplaceableWriteOutcome {
    Submitted,
    Suppressed,
}

struct ReplaceableWriteGate<K> {
    keys: Mutex<HashMap<K, Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>>>,
}

impl<K> Default for ReplaceableWriteGate<K> {
    fn default() -> Self {
        Self {
            keys: Mutex::new(HashMap::new()),
        }
    }
}

impl<K> ReplaceableWriteGate<K>
where
    K: Clone + Eq + Hash,
{
    fn key_state(
        &self,
        key: K,
        window: Duration,
    ) -> Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>> {
        let now = tokio::time::Instant::now();
        let mut keys = self
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(state) = keys.get(&key) {
            // A hot existing identity stays O(1), including at the cap. Full
            // scans are paid only by a caller attempting to admit a new key.
            return Arc::clone(state);
        }
        if keys.len() >= REPLACEABLE_WRITE_GATE_MAX_KEYS {
            keys.retain(|_, state| {
                if Arc::strong_count(state) > 1 {
                    return true;
                }
                state.try_lock().map_or(true, |last| {
                    last.is_some_and(|committed| now.duration_since(committed) < window)
                })
            });
        }
        if keys.len() >= REPLACEABLE_WRITE_GATE_MAX_KEYS {
            // Cardinality pressure may reduce coalescing efficiency, but must
            // never turn untrusted recipe identities into unbounded process
            // memory or suppress one identity using another identity's state.
            return Arc::new(tokio::sync::Mutex::new(None));
        }
        Arc::clone(
            keys.entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))),
        )
    }

    /// Serialize only equal identities. A waiter observes the first write's
    /// result before it may report suppression; failures leave no reservation
    /// and the next waiter retries instead of receiving an optimistic success.
    async fn run<F>(
        &self,
        key: K,
        window: Duration,
        operation: F,
    ) -> Result<ReplaceableWriteOutcome, StoreError>
    where
        F: Future<Output = Result<(), StoreError>>,
    {
        let state = self.key_state(key, window);
        let mut last_committed = state.lock().await;
        if last_committed
            .is_some_and(|committed| tokio::time::Instant::now().duration_since(committed) < window)
        {
            return Ok(ReplaceableWriteOutcome::Suppressed);
        }
        // Anchor the durability window before dispatch. A slow quorum write
        // must consume its own latency budget instead of extending a nominal
        // five-second cache window to five seconds after acknowledgement.
        let admitted_at = tokio::time::Instant::now();
        operation.await?;
        *last_committed = Some(admitted_at);
        Ok(ReplaceableWriteOutcome::Submitted)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ActivityCredential {
    Token(String),
    ApiKey(i64),
}

#[derive(Default)]
struct ActivityRefreshGate {
    reservations: Mutex<HashMap<ActivityCredential, i64>>,
}

impl ActivityRefreshGate {
    fn try_reserve(
        self: &Arc<Self>,
        credential: ActivityCredential,
        now: i64,
    ) -> Option<ActivityRefreshReservation> {
        let mut reservations = self
            .reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reservations
            .retain(|_, reserved_at| !crate::auth::activity_refresh_due(Some(*reserved_at), now));
        if reservations.contains_key(&credential) {
            return None;
        }
        reservations.insert(credential.clone(), now);
        Some(ActivityRefreshReservation {
            gate: Arc::clone(self),
            credential,
            reserved_at: now,
            retained: false,
        })
    }

    fn release(&self, credential: &ActivityCredential, reserved_at: i64) {
        let mut reservations = self
            .reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if reservations.get(credential) == Some(&reserved_at) {
            reservations.remove(credential);
        }
    }

    async fn run<F>(
        self: &Arc<Self>,
        credential: ActivityCredential,
        now: i64,
        operation: F,
    ) -> Result<(), StoreError>
    where
        F: Future<Output = Result<(), StoreError>>,
    {
        let Some(reservation) = self.try_reserve(credential, now) else {
            return Ok(());
        };
        let outcome = operation.await;
        if outcome.is_ok() {
            reservation.retain();
        }
        outcome
    }
}

struct ActivityRefreshReservation {
    gate: Arc<ActivityRefreshGate>,
    credential: ActivityCredential,
    reserved_at: i64,
    retained: bool,
}

impl ActivityRefreshReservation {
    fn retain(mut self) {
        self.retained = true;
    }
}

impl Drop for ActivityRefreshReservation {
    fn drop(&mut self) {
        if !self.retained {
            self.gate.release(&self.credential, self.reserved_at);
        }
    }
}

#[derive(Clone, Copy)]
enum StoreOperationClass {
    LocalRead,
    AuthorityRead,
    Write,
}

impl StoreOperationClass {
    const ALL: [Self; 3] = [Self::LocalRead, Self::AuthorityRead, Self::Write];

    fn index(self) -> usize {
        match self {
            Self::LocalRead => 0,
            Self::AuthorityRead => 1,
            Self::Write => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LocalRead => "local_read",
            Self::AuthorityRead => "authority_read",
            Self::Write => "write",
        }
    }
}

#[derive(Clone, Copy)]
enum StoreOperationOutcome {
    Ok,
    Error,
    Cancelled,
}

impl StoreOperationOutcome {
    const ALL: [Self; 3] = [Self::Ok, Self::Error, Self::Cancelled];

    fn index(self) -> usize {
        match self {
            Self::Ok => 0,
            Self::Error => 1,
            Self::Cancelled => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

const STORE_OPERATION_BUCKETS: [(u64, &str); 12] = [
    (1_000_000, "0.001"),
    (5_000_000, "0.005"),
    (10_000_000, "0.01"),
    (25_000_000, "0.025"),
    (50_000_000, "0.05"),
    (100_000_000, "0.1"),
    (250_000_000, "0.25"),
    (500_000_000, "0.5"),
    (1_000_000_000, "1"),
    (2_500_000_000, "2.5"),
    (5_000_000_000, "5"),
    (10_000_000_000, "10"),
];

#[derive(Default)]
struct StoreOperationCell {
    count: AtomicU64,
    elapsed_nanos: AtomicU64,
    buckets: [AtomicU64; STORE_OPERATION_BUCKETS.len()],
}

struct StoreOperationMetrics {
    cells: [StoreOperationCell; 9],
}

impl Default for StoreOperationMetrics {
    fn default() -> Self {
        Self {
            cells: std::array::from_fn(|_| StoreOperationCell::default()),
        }
    }
}

impl StoreOperationMetrics {
    fn saturating_add(target: &AtomicU64, amount: u64) {
        let _ = target.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != u64::MAX).then(|| current.saturating_add(amount))
        });
    }

    fn cell(
        &self,
        class: StoreOperationClass,
        outcome: StoreOperationOutcome,
    ) -> &StoreOperationCell {
        &self.cells[class.index() * StoreOperationOutcome::ALL.len() + outcome.index()]
    }

    fn record(
        &self,
        class: StoreOperationClass,
        outcome: StoreOperationOutcome,
        elapsed: Duration,
    ) {
        let elapsed_nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        let cell = self.cell(class, outcome);
        Self::saturating_add(&cell.count, 1);
        Self::saturating_add(&cell.elapsed_nanos, elapsed_nanos);
        if let Some(index) = STORE_OPERATION_BUCKETS
            .iter()
            .position(|(upper, _)| elapsed_nanos <= *upper)
        {
            Self::saturating_add(&cell.buckets[index], 1);
        }
    }

    fn render(&self) -> String {
        use std::fmt::Write;

        let mut out = String::from(
            "# HELP plurx_store_operation_seconds Replicated Store operation latency by consistency class and outcome.\n\
             # TYPE plurx_store_operation_seconds histogram\n",
        );
        for class in StoreOperationClass::ALL {
            for outcome in StoreOperationOutcome::ALL {
                let cell = self.cell(class, outcome);
                let mut cumulative = 0_u64;
                for (index, (_, upper)) in STORE_OPERATION_BUCKETS.iter().enumerate() {
                    cumulative =
                        cumulative.saturating_add(cell.buckets[index].load(Ordering::Relaxed));
                    let _ = writeln!(
                        out,
                        "plurx_store_operation_seconds_bucket{{class=\"{}\",outcome=\"{}\",le=\"{}\"}} {}",
                        class.label(),
                        outcome.label(),
                        upper,
                        cumulative,
                    );
                }
                let count = cell.count.load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "plurx_store_operation_seconds_bucket{{class=\"{}\",outcome=\"{}\",le=\"+Inf\"}} {}",
                    class.label(),
                    outcome.label(),
                    count,
                );
                let elapsed_nanos = cell.elapsed_nanos.load(Ordering::Relaxed);
                let seconds = elapsed_nanos as f64 / 1_000_000_000.0;
                let _ = writeln!(
                    out,
                    "plurx_store_operation_seconds_sum{{class=\"{}\",outcome=\"{}\"}} {seconds:.9}",
                    class.label(),
                    outcome.label(),
                );
                let _ = writeln!(
                    out,
                    "plurx_store_operation_seconds_count{{class=\"{}\",outcome=\"{}\"}} {count}",
                    class.label(),
                    outcome.label(),
                );
            }
        }
        out.push_str(
            "# HELP plurx_store_operations_total Replicated Store operations by consistency class and outcome.\n\
             # TYPE plurx_store_operations_total counter\n",
        );
        for class in StoreOperationClass::ALL {
            for outcome in StoreOperationOutcome::ALL {
                let count = self.cell(class, outcome).count.load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "plurx_store_operations_total{{class=\"{}\",outcome=\"{}\"}} {count}",
                    class.label(),
                    outcome.label(),
                );
            }
        }
        out
    }
}

static STORE_OPERATION_METRICS: LazyLock<StoreOperationMetrics> =
    LazyLock::new(StoreOperationMetrics::default);

// The named P2f runner needs a production-equivalent control arm without
// maintaining or rebuilding a historical binary. This switch exists only in
// the cluster validation build; shipped binaries compile the instrumented path
// unconditionally and expose no runtime way to disable their metrics.
#[cfg(feature = "cluster-read-cost-validation")]
static STORE_OPERATION_INSTRUMENTATION_ENABLED: AtomicBool = AtomicBool::new(true);

#[cfg(feature = "cluster-read-cost-validation")]
#[doc(hidden)]
pub fn validation_set_store_operation_instrumentation(enabled: bool) {
    STORE_OPERATION_INSTRUMENTATION_ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(feature = "cluster-read-cost-validation")]
#[doc(hidden)]
#[must_use]
pub fn validation_store_operation_instrumentation_enabled() -> bool {
    STORE_OPERATION_INSTRUMENTATION_ENABLED.load(Ordering::Relaxed)
}

#[cfg(feature = "cluster-read-cost-validation")]
#[doc(hidden)]
#[must_use]
pub fn validation_store_operation_metric_count() -> u64 {
    STORE_OPERATION_METRICS
        .cells
        .iter()
        .fold(0_u64, |total, cell| {
            total.saturating_add(cell.count.load(Ordering::Relaxed))
        })
}

struct StoreOperationTimer {
    metrics: &'static StoreOperationMetrics,
    class: StoreOperationClass,
    started_at: Instant,
    completed: bool,
}

impl StoreOperationTimer {
    fn start(metrics: &'static StoreOperationMetrics, class: StoreOperationClass) -> Self {
        Self {
            metrics,
            class,
            started_at: Instant::now(),
            completed: false,
        }
    }

    fn complete(mut self, outcome: StoreOperationOutcome) {
        self.metrics
            .record(self.class, outcome, self.started_at.elapsed());
        self.completed = true;
    }
}

impl Drop for StoreOperationTimer {
    fn drop(&mut self) {
        if !self.completed {
            self.metrics.record(
                self.class,
                StoreOperationOutcome::Cancelled,
                self.started_at.elapsed(),
            );
        }
    }
}

async fn time_store_operation<T>(
    metrics: &'static StoreOperationMetrics,
    class: StoreOperationClass,
    operation: impl Future<Output = Result<T, StoreError>>,
    successful: impl FnOnce(&T) -> bool,
) -> Result<T, StoreError> {
    #[cfg(feature = "cluster-read-cost-validation")]
    {
        time_store_operation_controlled(
            metrics,
            class,
            operation,
            successful,
            validation_store_operation_instrumentation_enabled(),
        )
        .await
    }
    #[cfg(not(feature = "cluster-read-cost-validation"))]
    {
        let timer = StoreOperationTimer::start(metrics, class);
        let result = operation.await;
        timer.complete(match result.as_ref() {
            Ok(value) if successful(value) => StoreOperationOutcome::Ok,
            Ok(_) | Err(_) => StoreOperationOutcome::Error,
        });
        result
    }
}

#[cfg(feature = "cluster-read-cost-validation")]
async fn time_store_operation_controlled<T>(
    metrics: &'static StoreOperationMetrics,
    class: StoreOperationClass,
    operation: impl Future<Output = Result<T, StoreError>>,
    successful: impl FnOnce(&T) -> bool,
    enabled: bool,
) -> Result<T, StoreError> {
    if !enabled {
        return operation.await;
    }
    let timer = StoreOperationTimer::start(metrics, class);
    let result = operation.await;
    timer.complete(match result.as_ref() {
        Ok(value) if successful(value) => StoreOperationOutcome::Ok,
        Ok(_) | Err(_) => StoreOperationOutcome::Error,
    });
    result
}

fn is_retryable_authority_read_error<T>(result: &Result<T, StoreError>) -> bool {
    match result {
        Err(StoreError::Database(message)) => {
            message == REPLICATED_STORE_TIMEOUT
                || (message.starts_with("CheckIsLeaderError:")
                    && message.contains("not enough for a quorum"))
        }
        _ => false,
    }
}

fn is_replicated_store_timeout<T>(result: &Result<T, StoreError>) -> bool {
    matches!(
        result,
        Err(StoreError::Database(message)) if message == REPLICATED_STORE_TIMEOUT
    )
}

async fn time_authority_read_with_retry<T, F, Fut>(
    metrics: &'static StoreOperationMetrics,
    mut operation: F,
) -> Result<T, StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    for attempt in 1..=AUTHORITY_READ_MAX_ATTEMPTS {
        let result = time_store_operation(
            metrics,
            StoreOperationClass::AuthorityRead,
            operation(),
            |_| true,
        )
        .await;
        if !is_retryable_authority_read_error(&result) || attempt == AUTHORITY_READ_MAX_ATTEMPTS {
            return result;
        }
        tracing::warn!(
            attempt,
            max_attempts = AUTHORITY_READ_MAX_ATTEMPTS,
            "transient replicated authority read failed; retrying"
        );
        tokio::time::sleep(AUTHORITY_READ_RETRY_DELAY).await;
    }
    unreachable!("the bounded authority-read retry loop always returns")
}

/// Retry one exact-state mutation only while the client reports its bounded
/// per-attempt deadline.
///
/// A timed-out consensus request is ambiguous: it may still have committed.
/// Callers must therefore supply an operation whose repeated execution writes
/// byte-for-byte equivalent durable state. Three three-second attempts plus
/// the two short delays stay inside the accepted ten-second leader-election
/// recovery window without relaxing [`STORE_TIMEOUT`] for any other call.
async fn time_idempotent_write_with_retry<T, F, Fut>(
    metrics: &'static StoreOperationMetrics,
    mut operation: F,
) -> Result<T, StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    for attempt in 1..=IDEMPOTENT_WRITE_MAX_ATTEMPTS {
        let result =
            time_store_operation(metrics, StoreOperationClass::Write, operation(), |_| true).await;
        if !is_replicated_store_timeout(&result) || attempt == IDEMPOTENT_WRITE_MAX_ATTEMPTS {
            return result;
        }
        tracing::warn!(
            attempt,
            max_attempts = IDEMPOTENT_WRITE_MAX_ATTEMPTS,
            "idempotent replicated write timed out; retrying exact state"
        );
        tokio::time::sleep(IDEMPOTENT_WRITE_RETRY_DELAY).await;
    }
    unreachable!("the bounded idempotent-write retry loop always returns")
}

/// Render fixed-cardinality process metrics for all replicated Store calls.
///
/// SQLite mode leaves these series at zero. Rendering reads only atomics and
/// cannot execute or wait on the Store operation it describes.
pub fn prometheus_store_operations() -> String {
    STORE_OPERATION_METRICS.render()
}

/// The only application-facing path to hiqlite. Keeping the timeout at this
/// boundary prevents new catalogue or durable-store calls from accidentally
/// waiting forever on a wedged leader.
#[derive(Clone)]
pub(super) struct TimedClient {
    inner: TimedClientInner,
    #[cfg(feature = "cluster-read-cost-validation")]
    operations: Arc<OperationCounters>,
}

#[derive(Clone)]
enum TimedClientInner {
    Connected(Client),
    #[cfg(test)]
    Disconnected,
}

impl TimedClient {
    fn new(client: Client) -> Self {
        Self {
            inner: TimedClientInner::Connected(client),
            #[cfg(feature = "cluster-read-cost-validation")]
            operations: Arc::new(OperationCounters::default()),
        }
    }

    fn inner(&self) -> &Client {
        match &self.inner {
            TimedClientInner::Connected(client) => client,
            #[cfg(test)]
            TimedClientInner::Disconnected => {
                panic!("validation test attempted hiqlite I/O")
            }
        }
    }

    pub(super) async fn query_consistent_map<T, S>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<Vec<T>, StoreError>
    where
        T: for<'a, 'r> From<&'a mut hiqlite::Row<'r>> + Send + 'static,
        S: Into<Cow<'static, str>>,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        time_authority_read_with_retry(&STORE_OPERATION_METRICS, || {
            #[cfg(feature = "cluster-read-cost-validation")]
            self.operations
                .consistent_query_calls
                .fetch_add(1, Ordering::Relaxed);
            timeout_store(
                self.inner()
                    .query_consistent_map(sql.clone(), params.clone()),
            )
        })
        .await
    }

    pub(super) async fn query_map<T, S>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<Vec<T>, StoreError>
    where
        T: for<'a, 'r> From<&'a mut hiqlite::Row<'r>> + Send + 'static,
        S: Into<Cow<'static, str>>,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        #[cfg(feature = "cluster-read-cost-validation")]
        self.operations
            .non_consistent_query_calls
            .fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "cluster-read-cost-validation")]
        if self
            .operations
            .fail_next_non_consistent_query
            .swap(false, Ordering::Relaxed)
        {
            return Err(StoreError::Task(
                "injected bounded-replica query failure".to_owned(),
            ));
        }
        time_store_operation(
            &STORE_OPERATION_METRICS,
            StoreOperationClass::LocalRead,
            timeout_store(self.inner().query_map(sql, params)),
            |_| true,
        )
        .await
    }

    pub(super) async fn execute<S>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<usize, StoreError>
    where
        S: Into<Cow<'static, str>>,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        #[cfg(feature = "cluster-read-cost-validation")]
        self.operations.write_calls.fetch_add(1, Ordering::Relaxed);
        time_store_operation(
            &STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            timeout_store(self.inner().execute(sql, params)),
            |_| true,
        )
        .await
    }

    pub(super) async fn execute_idempotent<S>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<usize, StoreError>
    where
        S: Into<Cow<'static, str>>,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        time_idempotent_write_with_retry(&STORE_OPERATION_METRICS, || {
            #[cfg(feature = "cluster-read-cost-validation")]
            self.operations.write_calls.fetch_add(1, Ordering::Relaxed);
            timeout_store(self.inner().execute(sql.clone(), params.clone()))
        })
        .await
    }

    pub(super) async fn execute_returning_map<S, T>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<Vec<Result<T, hiqlite::Error>>, StoreError>
    where
        S: Into<Cow<'static, str>>,
        T: for<'a, 'r> From<&'a mut hiqlite::Row<'r>> + Send + 'static,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        #[cfg(feature = "cluster-read-cost-validation")]
        self.operations.write_calls.fetch_add(1, Ordering::Relaxed);
        time_store_operation(
            &STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            timeout_store(self.inner().execute_returning_map(sql, params)),
            |rows| rows.iter().all(Result::is_ok),
        )
        .await
    }

    pub(super) async fn execute_returning_map_one<S, T>(
        &self,
        sql: S,
        params: hiqlite::Params,
    ) -> Result<T, StoreError>
    where
        S: Into<Cow<'static, str>>,
        T: for<'a, 'r> From<&'a mut hiqlite::Row<'r>> + Send + 'static,
    {
        let sql = sql.into();
        validate_sql(&sql)?;
        #[cfg(feature = "cluster-read-cost-validation")]
        self.operations.write_calls.fetch_add(1, Ordering::Relaxed);
        time_store_operation(
            &STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            timeout_store(self.inner().execute_returning_map_one(sql, params)),
            |_| true,
        )
        .await
    }

    pub(super) async fn txn<C, Q>(
        &self,
        statements: Q,
    ) -> Result<Vec<Result<usize, hiqlite::Error>>, StoreError>
    where
        Q: IntoIterator<Item = (C, hiqlite::Params)>,
        C: Into<Cow<'static, str>>,
    {
        let statements = statements
            .into_iter()
            .map(|(sql, params)| (sql.into(), params))
            .collect::<Vec<_>>();
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        #[cfg(feature = "cluster-read-cost-validation")]
        self.operations.write_calls.fetch_add(1, Ordering::Relaxed);
        time_store_operation(
            &STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            timeout_store(self.inner().txn(statements)),
            |results| results.iter().all(Result::is_ok),
        )
        .await
    }

    pub(super) async fn is_healthy_db(&self) -> Result<(), StoreError> {
        timeout_store(self.inner().is_healthy_db()).await
    }
}

#[cfg(test)]
pub(super) fn disconnected_test_client() -> TimedClient {
    TimedClient {
        inner: TimedClientInner::Disconnected,
        #[cfg(feature = "cluster-read-cost-validation")]
        operations: Arc::new(OperationCounters::default()),
    }
}

impl HiqliteAuthStore {
    pub(super) fn client(&self) -> &TimedClient {
        &self.client
    }

    /// Reset attempted client-call accounting before a validation workload.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub fn validation_reset_operation_counts(&self) {
        self.client.operations.reset();
    }

    /// Snapshot attempted client-call accounting after a validation workload.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub fn validation_operation_counts(&self) -> HiqliteOperationCounts {
        self.client.operations.snapshot()
    }

    /// Fail exactly the next local-query client call before network IO. This
    /// validation-only hook proves the catalogue boundary retries Authority
    /// rather than exposing a new application-visible error.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub fn validation_fail_next_non_consistent_query(&self) {
        self.client
            .operations
            .fail_next_non_consistent_query
            .store(true, Ordering::Relaxed);
    }

    /// Snapshot successful calls from the production metrics recorder. The
    /// validation contract compares deltas while it exclusively owns its
    /// three-voter fixture; production exposition remains process-wide.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub fn validation_successful_metric_counts(&self) -> HiqliteOperationCounts {
        HiqliteOperationCounts {
            consistent_query_calls: STORE_OPERATION_METRICS
                .cell(
                    StoreOperationClass::AuthorityRead,
                    StoreOperationOutcome::Ok,
                )
                .count
                .load(Ordering::Relaxed),
            non_consistent_query_calls: STORE_OPERATION_METRICS
                .cell(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok)
                .count
                .load(Ordering::Relaxed),
            write_calls: STORE_OPERATION_METRICS
                .cell(StoreOperationClass::Write, StoreOperationOutcome::Ok)
                .count
                .load(Ordering::Relaxed),
        }
    }

    /// Current production-metric count for statement-level write failures.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub fn validation_failed_write_metric_count(&self) -> u64 {
        STORE_OPERATION_METRICS
            .cell(StoreOperationClass::Write, StoreOperationOutcome::Error)
            .count
            .load(Ordering::Relaxed)
    }

    /// Submit a valid transaction whose statement violates the seeded
    /// instance-id uniqueness constraint. Hiqlite returns this as an inner
    /// statement error, which the production timer must classify as failed.
    #[cfg(feature = "cluster-read-cost-validation")]
    #[doc(hidden)]
    pub async fn validation_duplicate_instance_id_transaction(&self) -> Result<(), StoreError> {
        let results = self
            .client()
            .txn([(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3)",
                params!(keys::INSTANCE_ID, "duplicate", self.now()?),
            )])
            .await?;
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    /// Create the complete durable schema on a fresh cluster and seed its
    /// logical identity.
    ///
    /// Only the bootstrap coordinator calls this. Other voters call [`open`]
    /// after the acknowledged schema write has replicated.
    pub async fn bootstrap(
        client: Client,
        instance_id: &str,
        telemetry_path: &Path,
    ) -> Result<Self, StoreError> {
        Self::bootstrap_with_clock(client, instance_id, telemetry_path, Arc::new(SystemClock)).await
    }

    /// Bootstrap with a fixed clock for deterministic separate-process
    /// replicated-store validation. Production callers use [`Self::bootstrap`].
    #[doc(hidden)]
    pub async fn validation_bootstrap_at(
        client: Client,
        instance_id: &str,
        telemetry_path: &Path,
        now: i64,
    ) -> Result<Self, StoreError> {
        Self::bootstrap_with_clock(
            client,
            instance_id,
            telemetry_path,
            Arc::new(FixedClock(now)),
        )
        .await
    }

    async fn bootstrap_with_clock(
        client: Client,
        instance_id: &str,
        telemetry_path: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        validate_sql(AUTH_SCHEMA)?;
        let results = timeout_store(client.batch(AUTH_SCHEMA)).await?;
        for result in results {
            result.map_err(database_error)?;
        }
        super::hiqlite_catalog::install_schema(&client).await?;
        super::hiqlite_durable::install_schema(&client).await?;
        super::hiqlite_pretranscode::install_schema(&client).await?;
        super::hiqlite_sessions::install_schema(&client).await?;
        super::hiqlite_shared_cache::install_schema(&client).await?;
        super::hiqlite_fragment_index_cluster::install_schema(&client).await?;

        let store = Self::with_clock(client, clock, NodeLocalTelemetry::open(telemetry_path)?);
        let now = store.now()?;
        // A fresh cluster starts on the oldest protocol this binary supports,
        // not the newest. Bootstrapping is not consent to activate protocol 5:
        // a brand new cluster must stay joinable by the previous release until
        // an operator explicitly narrows the active range.
        store
            .execute(
                "INSERT INTO cluster_meta \
                 (singleton, schema_version, protocol_min, protocol_max, migrated_at) \
                 VALUES (1, $1, $2, $2, $3) \
                 ON CONFLICT(singleton) DO NOTHING",
                params!(AUTH_SCHEMA_VERSION, AUTH_PROTOCOL_MIN, now),
            )
            .await?;
        store
            .verify_compatibility(ClusterCompatibility::CURRENT)
            .await?;
        let now = store.now()?;
        store
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
                 ON CONFLICT(key) DO NOTHING",
                params!(keys::INSTANCE_ID, instance_id, now),
            )
            .await?;
        let persisted = store.instance_id().await?;
        if persisted != instance_id {
            return Err(StoreError::Identity(format!(
                "cluster instance.id is {persisted}, refusing bootstrap as {instance_id}"
            )));
        }
        Ok(store)
    }

    /// Open an already-bootstrapped cluster, refusing incompatible state.
    pub async fn open(client: Client, telemetry_path: &Path) -> Result<Self, StoreError> {
        let store = Self::with_clock(
            client,
            Arc::new(SystemClock),
            NodeLocalTelemetry::open(telemetry_path)?,
        );
        store
            .verify_compatibility(ClusterCompatibility::CURRENT)
            .await?;
        Ok(store)
    }

    /// Open the daemon's already-activated cluster, applying every supported
    /// additive schema step through the replicated state machine first.
    ///
    /// This path is intentionally separate from [`open`]: maintenance clients
    /// attach to a running voter and must never become a second migration
    /// coordinator. Every version step is one Raft transaction: a crash leaves
    /// either the prior version or a complete next version, and the next boot
    /// resumes the chain from the durable marker.
    pub async fn open_or_migrate(
        client: Client,
        telemetry_path: &Path,
    ) -> Result<Self, StoreError> {
        let store = Self::with_clock(
            client,
            Arc::new(SystemClock),
            NodeLocalTelemetry::open(telemetry_path)?,
        );
        store.migrate_schema().await?;
        store
            .verify_compatibility(ClusterCompatibility::CURRENT)
            .await?;
        Ok(store)
    }

    async fn migrate_schema(&self) -> Result<(), StoreError> {
        loop {
            let sql = "SELECT schema_version, protocol_min, protocol_max \
                       FROM cluster_meta WHERE singleton = 1";
            let rows = self
                .client()
                .query_consistent_map::<CompatibilityRow, _>(sql, params!())
                .await?;
            match schema_migration_action(&rows, ClusterCompatibility::CURRENT)? {
                SchemaMigrationAction::Current => return Ok(()),
                SchemaMigrationAction::MigrateFrom(AUTH_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let attempt = self
                        .client()
                        .txn([
                            (
                                super::hiqlite_catalog::READING_STATE_TABLE_SCHEMA,
                                params!(),
                            ),
                            (
                                super::hiqlite_catalog::READING_STATE_INDEX_SCHEMA,
                                params!(),
                            ),
                            (
                                "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                                 WHERE singleton = 1 AND schema_version = $3",
                                params!(READING_SCHEMA_VERSION, now, AUTH_SCHEMA_MIGRATION_SOURCE),
                            ),
                        ])
                        .await;
                    self.settle_migration_attempt(AUTH_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(BOOK_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let attempt = self
                        .client()
                        .txn([
                            (super::hiqlite_catalog::BOOK_AUTHOR_SCHEMA, params!()),
                            (super::hiqlite_catalog::BOOK_WORK_SCHEMA, params!()),
                            (super::hiqlite_catalog::BOOK_EDITION_SCHEMA, params!()),
                            (super::hiqlite_catalog::BOOK_SOURCE_SCHEMA, params!()),
                            (super::hiqlite_catalog::BOOK_WORK_INDEX_SCHEMA, params!()),
                            (
                                "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                                 WHERE singleton = 1 AND schema_version = $3",
                                params!(
                                    LEASE_SCHEMA_MIGRATION_SOURCE,
                                    now,
                                    BOOK_SCHEMA_MIGRATION_SOURCE
                                ),
                            ),
                        ])
                        .await;
                    self.settle_migration_attempt(BOOK_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(LEASE_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let attempt = self
                        .client()
                        .txn([
                            (super::hiqlite_coordination::JOB_LEASES_SCHEMA, params!()),
                            (
                                "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                                 WHERE singleton = 1 AND schema_version = $3",
                                params!(
                                    PRETRANSCODE_SCHEMA_MIGRATION_SOURCE,
                                    now,
                                    LEASE_SCHEMA_MIGRATION_SOURCE
                                ),
                            ),
                        ])
                        .await;
                    self.settle_migration_attempt(LEASE_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let attempt = self
                        .client()
                        .txn([
                            (
                                super::hiqlite_pretranscode::CACHE_MANIFEST_DIGEST_MIGRATION,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::CACHE_SCRUB_CURSOR_MIGRATION,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_JOBS_SCHEMA,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_DUE_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_DEDUPE_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_STAGING_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_ACTIVE_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_pretranscode::PRETRANSCODE_SOURCE_TRIGGER,
                                params!(),
                            ),
                            (
                                "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                                 WHERE singleton = 1 AND schema_version = $3",
                                params!(
                                    MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE,
                                    now,
                                    PRETRANSCODE_SCHEMA_MIGRATION_SOURCE
                                ),
                            ),
                        ])
                        .await;
                    self.settle_migration_attempt(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let attempt = self
                        .client()
                        .txn([
                            (
                                super::hiqlite_sessions::MEDIA_SESSION_REQUESTS_SCHEMA,
                                params!(),
                            ),
                            (
                                super::hiqlite_sessions::MEDIA_SESSION_REQUESTS_EXPIRY_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_sessions::MEDIA_PLAYBACK_POINTERS_SCHEMA,
                                params!(),
                            ),
                            (super::hiqlite_sessions::MEDIA_SESSIONS_SCHEMA, params!()),
                            (
                                super::hiqlite_sessions::MEDIA_SESSIONS_OWNER_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_sessions::MEDIA_SESSIONS_USER_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_sessions::MEDIA_SESSIONS_EXPIRY_INDEX,
                                params!(),
                            ),
                            (
                                super::hiqlite_sessions::MEDIA_SESSIONS_RETENTION_INDEX,
                                params!(),
                            ),
                            (
                                "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                                 WHERE singleton = 1 AND schema_version = $3",
                                params!(
                                    SHARED_CACHE_SCHEMA_MIGRATION_SOURCE,
                                    now,
                                    MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE
                                ),
                            ),
                        ])
                        .await;
                    self.settle_migration_attempt(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE) => {
                    let now = self.now()?;
                    let mut statements = super::hiqlite_shared_cache::migration_statements();
                    statements.push((
                        "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                         WHERE singleton = 1 AND schema_version = $3"
                            .to_owned(),
                        params!(
                            FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE,
                            now,
                            SHARED_CACHE_SCHEMA_MIGRATION_SOURCE
                        ),
                    ));
                    let attempt = self.client().txn(statements).await;
                    self.settle_migration_attempt(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE, attempt)
                        .await?;
                }
                SchemaMigrationAction::MigrateFrom(FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE) => {
                    // Every statement is additive and IF NOT EXISTS, making a
                    // concurrent coordinator harmless. Publish the marker only
                    // after the complete schema batch is quorum-applied.
                    super::hiqlite_fragment_index_cluster::install_schema(self.client().inner())
                        .await?;
                    let now = self.now()?;
                    self.execute(
                        "UPDATE cluster_meta SET schema_version = $1, migrated_at = $2 \
                         WHERE singleton = 1 AND schema_version = $3",
                        params!(
                            AUTH_SCHEMA_VERSION,
                            now,
                            FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE
                        ),
                    )
                    .await?;
                }
                SchemaMigrationAction::MigrateFrom(version) => {
                    return Err(StoreError::Migration(format!(
                        "cluster schema {version} has no migration implementation"
                    )));
                }
            }
        }
    }

    /// A second voter can observe the same predecessor before the first
    /// migration transaction commits. Additive DDL is not uniformly
    /// idempotent (`ALTER TABLE ADD COLUMN` in particular), so the losing
    /// coordinator may receive a statement error even though the cluster is
    /// now current. Treat an error as commit/concurrency-unknown, reread the
    /// replicated marker consistently, and suppress it only when another
    /// transaction durably advanced beyond the exact predecessor we tried.
    async fn settle_migration_attempt(
        &self,
        predecessor: i64,
        attempt: Result<Vec<Result<usize, hiqlite::Error>>, StoreError>,
    ) -> Result<(), StoreError> {
        let failure = match attempt {
            Ok(results) => results
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .err()
                .map(database_error),
            Err(error) => Some(error),
        };
        let Some(failure) = failure else {
            return Ok(());
        };
        let sql = "SELECT schema_version, protocol_min, protocol_max \
                   FROM cluster_meta WHERE singleton = 1";
        let rows = self
            .client()
            .query_consistent_map::<CompatibilityRow, _>(sql, params!())
            .await?;
        let version = rows
            .first()
            .filter(|_| rows.len() == 1)
            .map(|row| row.schema_version)
            .ok_or_else(|| {
                StoreError::Migration(
                    "cluster compatibility marker disappeared during migration".to_owned(),
                )
            })?;
        if version != predecessor {
            tracing::info!(
                predecessor,
                version,
                "another voter completed the replicated schema migration"
            );
            Ok(())
        } else {
            Err(failure)
        }
    }

    /// Run the compatibility guard through a remote client *before* starting
    /// this process's raft voter. A rejected process therefore cannot migrate
    /// state, vote, or become leader.
    pub async fn preflight_voter(
        remote: &Client,
        supported: ClusterCompatibility,
    ) -> Result<(), StoreError> {
        Self::preflight_role(remote, supported, false).await
    }

    /// The same guard, plus the one extra thing a learner has to prove: that
    /// the cluster has actually activated the protocol that admits one.
    ///
    /// The compatibility rule alone cannot catch this. A binary implementing
    /// `4..=5` covers an unactivated `4..=4` cluster perfectly well, so a
    /// learner would sail through the voter preflight and be refused later, by
    /// the coordinator, after this process had already decided it was joining.
    pub async fn preflight_role(
        remote: &Client,
        supported: ClusterCompatibility,
        learner: bool,
    ) -> Result<(), StoreError> {
        let sql = "SELECT schema_version, protocol_min, protocol_max \
                   FROM cluster_meta WHERE singleton = 1";
        validate_sql(sql)?;
        let meta =
            timeout_store(remote.query_consistent_map::<CompatibilityRow, _>(sql, params!()))
                .await?;
        let active = meta
            .first()
            .filter(|_| meta.len() == 1)
            .map(|row| (row.protocol_min, row.protocol_max));
        verify_compatibility_rows(meta, supported)?;
        if learner {
            let (active_min, active_max) = active.ok_or_else(|| {
                StoreError::Migration("cluster compatibility marker is missing".to_owned())
            })?;
            if !(active_min..=active_max).contains(&AUTH_LEARNER_PROTOCOL) {
                // Not a migration failure: nothing is being migrated and
                // nothing is broken. The cluster has simply not activated the
                // protocol that admits a learner, and prefixing this with
                // "schema migration failed" sent operators to the wrong place.
                return Err(StoreError::JoinRefused(format!(
                    "learner_protocol_inactive: this join token admits this node as a learner, \
                     but the cluster's active protocol range is {active_min}..={active_max} and \
                     does not include protocol {AUTH_LEARNER_PROTOCOL}; activate the learner \
                     protocol on the cluster first, then retry this join"
                )));
            }
        }
        Ok(())
    }

    pub async fn verify_compatibility(
        &self,
        supported: ClusterCompatibility,
    ) -> Result<(), StoreError> {
        let sql = "SELECT schema_version, protocol_min, protocol_max \
                   FROM cluster_meta WHERE singleton = 1";
        let meta = self
            .client()
            .query_consistent_map::<CompatibilityRow, _>(sql, params!())
            .await?;
        verify_compatibility_rows(meta, supported)
    }

    /// Hash ordered local state for the separate-process replica-equality gate.
    /// This is deliberately not a consistent leader read: the check needs to
    /// observe each voter's own applied SQLite state. Only the digest leaves
    /// this process; password and credential hashes never enter test logs.
    pub async fn local_dump_digest(&self) -> Result<String, StoreError> {
        let dump = self.local_auth_dump().await?;
        let bytes = serde_json::to_vec(&dump).map_err(database_error)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// Full validation payload used only by the separate-process gate. The
    /// controller independently hashes it and checks known fields so replica
    /// equality cannot certify a broken digest implementation.
    pub async fn validation_local_dump(&self) -> Result<String, StoreError> {
        serde_json::to_string(&self.local_auth_dump().await?).map_err(database_error)
    }

    /// Clear mutable application rows between backend-neutral contract cases.
    ///
    /// This is deliberately outside [`Store`](super::Store): production code
    /// must never gain a generic "erase the cluster" operation. The contract
    /// harness keeps the three voter processes alive and serializes calls to
    /// this validation-only helper so every scenario still starts empty.
    #[doc(hidden)]
    pub async fn validation_reset_contract_state(&self) -> Result<(), StoreError> {
        self.telemetry.clear().await?;
        let statements = vec![
            ("DELETE FROM media_playback_pointers".to_owned(), params!()),
            ("DELETE FROM media_sessions".to_owned(), params!()),
            ("DELETE FROM media_session_requests".to_owned(), params!()),
            ("DELETE FROM job_leases".to_owned(), params!()),
            ("DELETE FROM pretranscode_jobs".to_owned(), params!()),
            ("DELETE FROM offline_source_probes".to_owned(), params!()),
            ("DELETE FROM offline_lease_guards".to_owned(), params!()),
            ("DELETE FROM offline_package_leases".to_owned(), params!()),
            ("DELETE FROM offline_packages".to_owned(), params!()),
            ("DELETE FROM cache_consumer_pins".to_owned(), params!()),
            ("DELETE FROM cache_storage_members".to_owned(), params!()),
            (
                "DELETE FROM transcode_cache_locations".to_owned(),
                params!(),
            ),
            ("DELETE FROM transcode_cache_recipes".to_owned(), params!()),
            ("DELETE FROM watched_outbox".to_owned(), params!()),
            ("DELETE FROM trakt_auth".to_owned(), params!()),
            ("DELETE FROM watch_state".to_owned(), params!()),
            ("DELETE FROM reading_state".to_owned(), params!()),
            ("DELETE FROM scan_reconcile_items".to_owned(), params!()),
            ("DELETE FROM scan_reconcile_guards".to_owned(), params!()),
            ("DELETE FROM library_roots".to_owned(), params!()),
            ("DELETE FROM files".to_owned(), params!()),
            ("DELETE FROM items".to_owned(), params!()),
            ("DELETE FROM libraries".to_owned(), params!()),
            ("DELETE FROM tokens".to_owned(), params!()),
            ("DELETE FROM api_keys".to_owned(), params!()),
            ("DELETE FROM users".to_owned(), params!()),
            (
                "DELETE FROM settings WHERE key <> $1 \
                   AND key NOT GLOB 'internal.cluster_job_owner_removed.*'"
                    .to_owned(),
                params!(keys::INSTANCE_ID),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    /// Hash only authoritative catalogue tables from this voter. The cluster
    /// gate uses this to prove a damaged derived FTS index cannot change truth.
    pub async fn validation_local_catalog_truth_digest(&self) -> Result<String, StoreError> {
        tokio::time::timeout(
            STORE_TIMEOUT,
            super::hiqlite_catalog::local_catalog_truth_digest(self.client()),
        )
        .await
        .map_err(|_| StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))?
    }

    async fn local_auth_dump(&self) -> Result<AuthStoreDump, StoreError> {
        for sql in [
            "SELECT singleton, schema_version, protocol_min, protocol_max, migrated_at FROM cluster_meta ORDER BY singleton",
            "SELECT key, value, updated_at FROM settings ORDER BY key",
            "SELECT id, username, password_hash, is_admin, created_at FROM users ORDER BY id",
            "SELECT token_hash, user_id, device, created_at, last_seen_at FROM tokens ORDER BY token_hash",
            "SELECT id, name, key_hash, scopes, created_at, last_used_at, disabled FROM api_keys ORDER BY id",
            "SELECT resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms FROM job_leases ORDER BY resource",
            "SELECT user_id, request_id, request_fingerprint, playback_id, state, claim_expires_at_ms, incarnation_id, owner_node_id, response_json, updated_at_ms FROM media_session_requests ORDER BY user_id, request_id",
            "SELECT user_id, playback_id, current_incarnation_id, updated_at_ms FROM media_playback_pointers ORDER BY user_id, playback_id",
            "SELECT incarnation_id, session_id, user_id, playback_id, request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json, response_json, produced_playable_through_ms, fetched_through_ms, media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms FROM media_sessions ORDER BY incarnation_id",
        ] {
            validate_sql(sql)?;
        }
        Ok(AuthStoreDump {
            catalog_digest: tokio::time::timeout(
                STORE_TIMEOUT,
                super::hiqlite_catalog::local_catalog_digest(self.client()),
            )
            .await
            .map_err(|_| {
                StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())
            })??,
            durable_digest: tokio::time::timeout(
                STORE_TIMEOUT,
                super::hiqlite_durable::local_durable_digest(self.client()),
            )
            .await
            .map_err(|_| {
                StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())
            })??,
            cluster_meta: self.client().query_map(
                "SELECT singleton, schema_version, protocol_min, protocol_max, migrated_at \
                     FROM cluster_meta ORDER BY singleton",
                params!(),
            )
            .await?,
            settings: self.client().query_map(
                "SELECT key, value, updated_at FROM settings ORDER BY key",
                params!(),
            )
            .await?,
            users: self.client().query_map(
                "SELECT id, username, password_hash, is_admin, created_at \
                     FROM users ORDER BY id",
                params!(),
            )
            .await?,
            tokens: self.client().query_map(
                "SELECT token_hash, user_id, device, created_at, last_seen_at \
                     FROM tokens ORDER BY token_hash",
                params!(),
            )
            .await?,
            api_keys: self.client().query_map(
                "SELECT id, name, key_hash, scopes, created_at, last_used_at, disabled \
                     FROM api_keys ORDER BY id",
                params!(),
            )
            .await?,
            job_leases: self.client().query_map(
                "SELECT resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms \
                     FROM job_leases ORDER BY resource",
                params!(),
            )
            .await?,
            media_session_requests: self.client().query_map(
                "SELECT user_id, request_id, request_fingerprint, playback_id, state, \
                        claim_expires_at_ms, incarnation_id, owner_node_id, response_json, \
                        updated_at_ms \
                   FROM media_session_requests ORDER BY user_id, request_id",
                params!(),
            )
            .await?,
            media_playback_pointers: self.client().query_map(
                "SELECT user_id, playback_id, current_incarnation_id, updated_at_ms \
                   FROM media_playback_pointers ORDER BY user_id, playback_id",
                params!(),
            )
            .await?,
            media_sessions: self.client().query_map(
                "SELECT incarnation_id, session_id, user_id, playback_id, request_fingerprint, \
                        owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json, \
                        response_json, produced_playable_through_ms, fetched_through_ms, \
                        media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms \
                   FROM media_sessions ORDER BY incarnation_id",
                params!(),
            )
            .await?,
        })
    }

    fn with_clock(client: Client, clock: Arc<dyn Clock>, telemetry: NodeLocalTelemetry) -> Self {
        Self {
            client: TimedClient::new(client),
            clock,
            telemetry,
            activity_refreshes: Arc::new(ActivityRefreshGate::default()),
            cache_touches: Arc::new(ReplaceableWriteGate::default()),
        }
    }

    pub(super) async fn coalesce_cache_touch<F>(
        &self,
        key: CacheTouchKey,
        operation: F,
    ) -> Result<(), StoreError>
    where
        F: Future<Output = Result<(), StoreError>>,
    {
        self.cache_touches
            .run(key, CACHE_TOUCH_COMMIT_WINDOW, operation)
            .await
            .map(|_| ())
    }

    pub(super) fn now(&self) -> Result<i64, StoreError> {
        self.clock.now()
    }

    async fn refresh_token_activity_if_due(
        &self,
        token_hash: &str,
        observed_last_seen_at: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        if !crate::auth::activity_refresh_due(Some(observed_last_seen_at), now) {
            return Ok(());
        }
        let credential = ActivityCredential::Token(token_hash.to_owned());
        self.activity_refreshes
            .run(credential, now, async {
                let rows = self
                    .client()
                    .query_consistent_map::<ActivityTimestampRow, _>(
                        "SELECT last_seen_at AS last_activity_at \
                         FROM tokens WHERE token_hash = $1",
                        params!(token_hash),
                    )
                    .await?;
                let Some(current) = rows.into_iter().next() else {
                    return Ok(());
                };
                if crate::auth::activity_refresh_due(current.last_activity_at, now) {
                    self.execute(
                        "UPDATE tokens SET last_seen_at = $1 \
                         WHERE token_hash = $2 AND last_seen_at < $3",
                        params!(
                            now,
                            token_hash,
                            now.saturating_sub(crate::auth::ACTIVITY_REFRESH_SECS)
                        ),
                    )
                    .await?;
                }
                Ok(())
            })
            .await
    }

    async fn refresh_api_key_activity(&self, id: i64, now: i64) -> Result<(), StoreError> {
        let credential = ActivityCredential::ApiKey(id);
        self.activity_refreshes
            .run(credential, now, async {
                let rows = self
                    .client()
                    .query_consistent_map::<ActivityTimestampRow, _>(
                        "SELECT last_used_at AS last_activity_at \
                         FROM api_keys WHERE id = $1 AND disabled = 0",
                        params!(id),
                    )
                    .await?;
                let Some(current) = rows.into_iter().next() else {
                    return Ok(());
                };
                if crate::auth::activity_refresh_due(current.last_activity_at, now) {
                    self.execute(
                        "UPDATE api_keys SET last_used_at = $1 \
                         WHERE id = $2 AND disabled = 0 \
                           AND (last_used_at IS NULL OR last_used_at < $3)",
                        params!(
                            now,
                            id,
                            now.saturating_sub(crate::auth::ACTIVITY_REFRESH_SECS)
                        ),
                    )
                    .await?;
                }
                Ok(())
            })
            .await
    }

    pub(super) async fn execute(
        &self,
        sql: &'static str,
        params: hiqlite::Params,
    ) -> Result<usize, StoreError> {
        validate_sql(sql)?;
        self.client().execute(sql, params).await
    }

    async fn execute_idempotent(
        &self,
        sql: &'static str,
        params: hiqlite::Params,
    ) -> Result<usize, StoreError> {
        validate_sql(sql)?;
        self.client().execute_idempotent(sql, params).await
    }

    async fn user_optional(
        &self,
        sql: &'static str,
        params: hiqlite::Params,
    ) -> Result<Option<User>, StoreError> {
        validate_sql(sql)?;
        let mut rows = self
            .client()
            .query_consistent_map::<UserRow, _>(sql, params)
            .await?;
        Ok(rows.pop().map(Into::into))
    }

    async fn key_optional(
        &self,
        sql: &'static str,
        params: hiqlite::Params,
    ) -> Result<Option<ApiKey>, StoreError> {
        validate_sql(sql)?;
        let mut rows = self
            .client()
            .query_consistent_map::<ApiKeyRow, _>(sql, params)
            .await?;
        Ok(rows.pop().map(Into::into))
    }
}

#[async_trait]
impl crate::store::FragmentIndexStore for HiqliteAuthStore {
    async fn put_fragment_index(
        &self,
        file_id: i64,
        index: &crate::segplan::FragmentIndex,
    ) -> Result<(), StoreError> {
        let now_ms = self.clock.now()?;
        self.telemetry
            .put_fragment_index(file_id, index.clone(), now_ms)
            .await
    }

    async fn fragment_index(
        &self,
        file_id: i64,
        identity: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::FragmentIndex>, StoreError> {
        self.telemetry
            .fragment_index(file_id, identity.clone())
            .await
    }

    async fn forget_fragment_index(&self, file_id: i64) -> Result<bool, StoreError> {
        self.telemetry.forget_fragment_index(file_id).await
    }

    async fn vod_row_file_ids(&self, limit: i64) -> Result<Vec<i64>, StoreError> {
        // The sidecar's own rows -- this node's, which is the whole point.
        self.telemetry.vod_row_file_ids(limit).await
    }

    async fn surviving_file_ids(&self, file_ids: &[i64]) -> Result<Vec<i64>, StoreError> {
        // The replicated side. Every node agrees on this answer, which is why
        // the sweep converges rather than each node guessing -- and why a node
        // that was down for the delete still cleans up on its next tick.
        //
        // One query per id rather than an `IN` list: the caller's window is
        // small and bounded, and building an n-placeholder statement here
        // would be the only dynamic SQL in this file.
        let mut alive = Vec::new();
        for id in file_ids {
            let rows: Vec<IdRow> = self
                .client
                .query_map("SELECT id FROM files WHERE id = $1", hiqlite::params!(*id))
                .await?;
            if !rows.is_empty() {
                alive.push(*id);
            }
        }
        Ok(alive)
    }
}

#[async_trait]
impl crate::store::RenditionPlanStore for HiqliteAuthStore {
    async fn put_rendition_plan(
        &self,
        rendition_key: &str,
        file_id: i64,
        plan: &crate::segplan::SegmentPlan,
        source: &crate::segplan::SourceIdentity,
    ) -> Result<bool, StoreError> {
        let now_ms = self.clock.now()?;
        self.telemetry
            .put_rendition_plan(
                rendition_key.to_owned(),
                file_id,
                plan.clone(),
                source.clone(),
                now_ms,
            )
            .await
    }

    async fn rendition_plan(
        &self,
        rendition_key: &str,
        source: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::SegmentPlan>, StoreError> {
        self.telemetry
            .rendition_plan(rendition_key.to_owned(), source.clone())
            .await
    }

    async fn forget_rendition_plans(&self, file_id: i64) -> Result<usize, StoreError> {
        self.telemetry.forget_rendition_plans(file_id).await
    }
}

#[async_trait]
impl PlaybackTelemetryStore for HiqliteAuthStore {
    async fn record_playback_event(&self, event: &PlaybackEvent) -> Result<i64, StoreError> {
        self.telemetry.record(event.clone()).await
    }

    async fn prune_playback_events(&self, before_ms: i64, limit: i64) -> Result<u64, StoreError> {
        self.telemetry.prune(before_ms, limit).await
    }

    async fn playback_events(
        &self,
        query: &PlaybackEventQuery,
    ) -> Result<Vec<PlaybackEvent>, StoreError> {
        self.telemetry.events(query.clone()).await
    }
}

#[async_trait]
impl NetworkPriorStore for HiqliteAuthStore {
    async fn observe_network_prior(
        &self,
        observation: &NetworkPriorObservation,
    ) -> Result<NetworkPrior, StoreError> {
        self.telemetry.observe_prior(observation.clone()).await
    }

    async fn network_prior(
        &self,
        credential_generation: &str,
        client_class: &str,
        network_fingerprint: &str,
    ) -> Result<Option<NetworkPrior>, StoreError> {
        self.telemetry
            .prior(
                credential_generation.to_owned(),
                client_class.to_owned(),
                network_fingerprint.to_owned(),
            )
            .await
    }

    async fn prune_network_priors(&self, before_ms: i64, limit: i64) -> Result<u64, StoreError> {
        self.telemetry.prune_priors(before_ms, limit).await
    }
}

#[async_trait]
impl MetricsStore for HiqliteAuthStore {
    async fn prometheus_store_snapshot(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<PrometheusStoreSnapshot, StoreError> {
        let row = self
            .client()
            .query_consistent_map::<PrometheusStoreRow, _>(
                "SELECT \
                    (SELECT COUNT(*) FROM libraries) AS libraries, \
                    (SELECT COUNT(*) FROM users) AS users, \
                    COALESCE(SUM(state = 'queued'), 0) AS queued, \
                    COALESCE(SUM(state = 'preparing'), 0) AS preparing, \
                    COALESCE(SUM(state = 'ready'), 0) AS ready, \
                    COALESCE(SUM(state = 'failed'), 0) AS failed, \
                    COALESCE(SUM(CASE WHEN state = 'queued' \
                      THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0) AS queued_bytes, \
                    COALESCE(SUM(CASE WHEN state = 'preparing' \
                      THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0) AS preparing_bytes, \
                    COALESCE(SUM(CASE WHEN state = 'ready' \
                      THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0) AS ready_bytes, \
                    COALESCE(SUM(CASE WHEN state = 'failed' \
                      THEN COALESCE(actual_bytes, reserved_bytes) ELSE 0 END), 0) AS failed_bytes, \
                    (SELECT COUNT(*) FROM offline_package_leases lease \
                     JOIN offline_packages active ON active.id = lease.package_id \
                     WHERE active.node_id = $1 AND active.state = 'ready' \
                       AND lease.expires_at > $2) AS active_leases, \
                    (SELECT COALESCE(SUM(location.bytes), 0) \
                     FROM transcode_cache_locations location \
                     WHERE location.node_id = $1 AND location.storage_class = 'local' \
                       AND location.complete = 1 AND EXISTS ( \
                         SELECT 1 FROM offline_packages pinned \
                         WHERE pinned.node_id = location.node_id \
                           AND pinned.recipe_hash = location.recipe_hash \
                           AND pinned.state IN ('queued', 'preparing', 'ready'))) AS pinned_bytes, \
                    (SELECT COALESCE(SUM(status = 'pending'), 0) \
                     FROM watched_outbox) AS outbox_pending, \
                    (SELECT COALESCE(SUM(status = 'ok'), 0) \
                     FROM watched_outbox) AS outbox_ok, \
                    (SELECT COALESCE(SUM(status = 'failed'), 0) \
                     FROM watched_outbox) AS outbox_failed \
                 FROM offline_packages WHERE node_id = $1",
                params!(node_id, now),
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::Database("Prometheus snapshot returned no row".to_owned()))?;
        Ok(row.into())
    }
}

#[async_trait]
impl SettingsStore for HiqliteAuthStore {
    async fn ping(&self) -> Result<(), StoreError> {
        self.client().is_healthy_db().await?;
        let sql = "SELECT 1 AS healthy";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<PingRow, _>(sql, params!())
            .await?;
        if rows.len() == 1 && rows[0].healthy == 1 {
            Ok(())
        } else {
            Err(StoreError::Database(
                "replicated readiness query returned an invalid result".to_owned(),
            ))
        }
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        let sql = "SELECT value FROM settings WHERE key = $1";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<SettingValueRow, _>(sql, params!(key))
            .await?;
        Ok(rows.into_iter().next().map(|row| row.value))
    }

    async fn get_or_init_setting(&self, key: &str, seed: &str) -> Result<String, StoreError> {
        let now = self.now()?;
        self.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
             ON CONFLICT(key) DO NOTHING",
            params!(key, seed, now),
        )
        .await?;
        self.get_setting(key).await?.ok_or_else(|| {
            StoreError::Database(format!("setting {key} missing after initialization"))
        })
    }

    async fn get_setting_pair(
        &self,
        first: &str,
        second: &str,
    ) -> Result<(Option<String>, Option<String>), StoreError> {
        let sql = "SELECT key, value FROM settings WHERE key = $1 OR key = $2 ORDER BY key";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<SettingEntryRow, _>(sql, params!(first, second))
            .await?;
        let mut pair = (None, None);
        for row in rows {
            if row.key == first {
                pair.0 = Some(row.value.clone());
            }
            if row.key == second {
                pair.1 = Some(row.value);
            }
        }
        Ok(pair)
    }

    async fn settings_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        let sql = "SELECT key, value FROM settings ORDER BY key";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<SettingEntryRow, _>(sql, params!())
            .await?;
        Ok(rows.into_iter().map(|row| (row.key, row.value)).collect())
    }

    async fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let now = self.now()?;
        self.execute_idempotent(
            "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
             ON CONFLICT(key) DO UPDATE SET \
             value = excluded.value, updated_at = excluded.updated_at",
            params!(key, value, now),
        )
        .await?;
        Ok(())
    }

    async fn put_setting_if_absent(&self, key: &str, value: &str) -> Result<bool, StoreError> {
        let now = self.now()?;
        Ok(self
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
                 ON CONFLICT(key) DO NOTHING",
                params!(key, value, now),
            )
            .await?
            == 1)
    }

    async fn put_setting_if_absent_if_artwork_repair_current(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, StoreError> {
        let now = self.now()?;
        Ok(self
            .execute(
                "INSERT INTO settings (key, value, updated_at) \
                 SELECT $1, $2, $3 WHERE $4 = $5 AND EXISTS (\
                   SELECT 1 FROM cluster_artwork_repairs \
                   WHERE item_id = $5 AND owner_node_id = $6 AND leader_term = $7 \
                     AND generation = $8) \
                 ON CONFLICT(key) DO NOTHING",
                params!(
                    key,
                    value,
                    now,
                    expected_item_id,
                    fence.item_id,
                    fence.owner_node_id.as_str(),
                    fence.leader_term,
                    fence.generation
                ),
            )
            .await?
            == 1)
    }

    async fn prune_unreferenced_book_cover_origins(
        &self,
        filename: &str,
    ) -> Result<usize, StoreError> {
        self.execute(
            "DELETE FROM settings
              WHERE substr(key, 1, 27) = 'internal.book_cover_origin.'
                AND json_extract(CASE WHEN json_valid(value) THEN value ELSE '{}' END,
                                 '$.filename') = $1
                AND NOT EXISTS (
                    SELECT 1 FROM items WHERE poster_path = $1 OR backdrop_path = $1)",
            params!(filename),
        )
        .await
    }

    async fn put_settings(&self, values: &[(&str, &str)]) -> Result<(), StoreError> {
        let now = self.now()?;
        let sql = "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
                   ON CONFLICT(key) DO UPDATE SET \
                   value = excluded.value, updated_at = excluded.updated_at";
        validate_sql(sql)?;
        self.client()
            .txn(
                values
                    .iter()
                    .map(|(key, value)| (sql.to_owned(), params!(*key, *value, now))),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    async fn instance_id(&self) -> Result<String, StoreError> {
        self.get_setting(keys::INSTANCE_ID).await?.ok_or_else(|| {
            StoreError::Database("instance.id missing — migration invariant broken".to_owned())
        })
    }
}

#[async_trait]
impl UserStore for HiqliteAuthStore {
    async fn count_users(&self) -> Result<i64, StoreError> {
        let sql = "SELECT COUNT(*) AS count FROM users";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<CountRow, _>(sql, params!())
            .await?;
        one_count(rows)
    }

    async fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_admin: bool,
    ) -> Result<User, StoreError> {
        let now = self.now()?;
        let sql = "INSERT INTO users \
                   (username, password_hash, is_admin, created_at) \
                   VALUES ($1, $2, $3, $4) \
                   RETURNING id, username, password_hash, is_admin, created_at";
        validate_sql(sql)?;
        let row = self
            .client()
            .execute_returning_map_one::<_, UserRow>(
                sql,
                params!(username, password_hash, is_admin, now),
            )
            .await?;
        Ok(row.into())
    }

    async fn get_user(&self, id: i64) -> Result<Option<User>, StoreError> {
        self.user_optional(
            "SELECT id, username, password_hash, is_admin, created_at \
             FROM users WHERE id = $1",
            params!(id),
        )
        .await
    }

    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>, StoreError> {
        self.user_optional(
            "SELECT id, username, password_hash, is_admin, created_at \
             FROM users WHERE username = $1",
            params!(username),
        )
        .await
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let sql = "SELECT id, username, password_hash, is_admin, created_at \
                   FROM users ORDER BY username";
        validate_sql(sql)?;
        Ok(self
            .client()
            .query_consistent_map::<UserRow, _>(sql, params!())
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn list_users_page(&self, after_id: i64, limit: i64) -> Result<Vec<User>, StoreError> {
        if after_id < 0 || !(1..=256).contains(&limit) {
            return Err(StoreError::Task("invalid bounded user page".to_owned()));
        }
        let sql = "SELECT id, username, password_hash, is_admin, created_at \
                   FROM users WHERE id > $1 ORDER BY id LIMIT $2";
        validate_sql(sql)?;
        Ok(self
            .client()
            .query_consistent_map::<UserRow, _>(sql, params!(after_id, limit))
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn delete_user(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .execute("DELETE FROM users WHERE id = $1", params!(id))
            .await?
            > 0)
    }

    async fn count_admins(&self) -> Result<i64, StoreError> {
        let sql = "SELECT COUNT(*) AS count FROM users WHERE is_admin = 1";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<CountRow, _>(sql, params!())
            .await?;
        one_count(rows)
    }

    async fn set_password(&self, id: i64, password_hash: &str) -> Result<bool, StoreError> {
        // hiqlite binds by SQLite parameter index, and `$N` is a named
        // parameter whose index follows first appearance. Keep `$1` first in
        // the SQL even when the SET clause precedes the predicate.
        Ok(self
            .execute(
                "UPDATE users SET password_hash = $1 WHERE id = $2",
                params!(password_hash, id),
            )
            .await?
            > 0)
    }

    async fn set_admin(&self, id: i64, is_admin: bool) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE users SET is_admin = $1 WHERE id = $2",
                params!(is_admin, id),
            )
            .await?
            > 0)
    }

    async fn delete_tokens_for_user(&self, user_id: i64) -> Result<u64, StoreError> {
        Ok(self
            .execute("DELETE FROM tokens WHERE user_id = $1", params!(user_id))
            .await? as u64)
    }

    async fn create_token(
        &self,
        token_hash: &str,
        user_id: i64,
        device: Option<&str>,
    ) -> Result<(), StoreError> {
        let now = self.now()?;
        self.execute(
            "INSERT INTO tokens \
             (token_hash, user_id, device, created_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $4)",
            params!(token_hash, user_id, device, now),
        )
        .await?;
        Ok(())
    }

    async fn user_for_token(&self, token_hash: &str) -> Result<Option<User>, StoreError> {
        let sql = "SELECT u.id, u.username, u.password_hash, u.is_admin, u.created_at, \
                          t.last_seen_at \
                 FROM users u JOIN tokens t ON t.user_id = u.id \
                 WHERE t.token_hash = $1";
        validate_sql(sql)?;
        let mut rows = self
            .client()
            .query_consistent_map::<TokenUserRow, _>(sql, params!(token_hash))
            .await?;
        let row = rows.pop();
        if let Some(row) = row.as_ref() {
            let now = self.now()?;
            self.refresh_token_activity_if_due(token_hash, row.last_seen_at, now)
                .await?;
        }
        Ok(row.map(Into::into))
    }

    async fn delete_token(&self, token_hash: &str) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "DELETE FROM tokens WHERE token_hash = $1",
                params!(token_hash),
            )
            .await?
            > 0)
    }
}

#[async_trait]
impl ApiKeyStore for HiqliteAuthStore {
    async fn create_api_key(
        &self,
        name: &str,
        key_hash: &str,
        scopes: &[String],
    ) -> Result<ApiKey, StoreError> {
        let now = self.now()?;
        let scopes = serde_json::to_string(scopes)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let sql = "INSERT INTO api_keys \
                   (name, key_hash, scopes, created_at, disabled) \
                   VALUES ($1, $2, $3, $4, 0) \
                   RETURNING id, name, key_hash, scopes, created_at, last_used_at, disabled";
        validate_sql(sql)?;
        let row = self
            .client()
            .execute_returning_map_one::<_, ApiKeyRow>(sql, params!(name, key_hash, scopes, now))
            .await?;
        Ok(row.into())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StoreError> {
        let sql = "SELECT id, name, key_hash, scopes, created_at, last_used_at, disabled \
                   FROM api_keys ORDER BY created_at, id";
        validate_sql(sql)?;
        Ok(self
            .client()
            .query_consistent_map::<ApiKeyRow, _>(sql, params!())
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn api_key_for_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError> {
        self.key_optional(
            "SELECT id, name, key_hash, scopes, created_at, last_used_at, disabled \
             FROM api_keys WHERE key_hash = $1",
            params!(key_hash),
        )
        .await
    }

    async fn touch_api_key(&self, id: i64) -> Result<(), StoreError> {
        let now = self.now()?;
        self.refresh_api_key_activity(id, now).await
    }

    async fn delete_api_key(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .execute("DELETE FROM api_keys WHERE id = $1", params!(id))
            .await?
            > 0)
    }

    async fn set_api_key_disabled(&self, id: i64, disabled: bool) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE api_keys SET disabled = $1 WHERE id = $2",
                params!(disabled, id),
            )
            .await?
            > 0)
    }
}

trait Clock: Send + Sync {
    fn now(&self) -> Result<i64, StoreError>;
}

struct FixedClock(i64);

impl Clock for FixedClock {
    fn now(&self) -> Result<i64, StoreError> {
        Ok(self.0)
    }
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Result<i64, StoreError> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| StoreError::Task(format!("system clock before unix epoch: {error}")))?
            .as_secs();
        i64::try_from(seconds)
            .map_err(|_| StoreError::Task("system clock exceeds i64 unix seconds".to_owned()))
    }
}

pub(super) fn validate_sql(sql: &str) -> Result<(), StoreError> {
    ReplicatedSql::new(sql)
        .map(|_| ())
        .map_err(|error| StoreError::Database(error.to_string()))?;
    validate_parameter_order(sql)
}

/// hiqlite binds parameters with rusqlite's numeric parameter index. SQLite
/// treats `$N` as a *named* parameter, so its index is assigned by first
/// appearance rather than by the number after `$`. Requiring first appearance
/// to be `$1`, `$2`, ... keeps `params!(...)` aligned with the statement.
fn validate_parameter_order(sql: &str) -> Result<(), StoreError> {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut next = 1_u32;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if bytes.get(index + 1) == Some(&quote) {
                            index += 2;
                        } else {
                            index += 1;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            b'[' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b']' {
                        if bytes.get(index + 1) == Some(&b']') {
                            index += 2;
                        } else {
                            index += 1;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'$' if bytes.get(index + 1).is_some_and(u8::is_ascii_digit) => {
                let start = index + 1;
                index = start;
                while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
                let token = &sql[start..index];
                if token.len() > 1 && token.starts_with('0') {
                    return Err(StoreError::Database(format!(
                        "hiqlite placeholders must use canonical $N spelling; found ${token}"
                    )));
                }
                let ordinal = token
                    .parse::<u32>()
                    .map_err(|error| StoreError::Database(error.to_string()))?;
                if ordinal == next {
                    next += 1;
                } else if ordinal >= next || ordinal == 0 {
                    return Err(StoreError::Database(format!(
                        "hiqlite placeholders must first appear in order; expected ${next}, found ${ordinal}"
                    )));
                }
            }
            b'$' | b'?' | b':' | b'@' => {
                return Err(StoreError::Database(format!(
                    "hiqlite statements may only use canonical $N placeholders; found {:?}",
                    bytes[index] as char
                )));
            }
            _ => index += 1,
        }
    }
    Ok(())
}

pub(super) fn database_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::Database(error.to_string())
}

pub(super) async fn timeout_store<T, E>(
    operation: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, StoreError>
where
    E: std::fmt::Display,
{
    tokio::time::timeout(STORE_TIMEOUT, operation)
        .await
        .map_err(|_| StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))?
        .map_err(database_error)
}

fn one_count(rows: Vec<CountRow>) -> Result<i64, StoreError> {
    if rows.len() == 1 {
        Ok(rows[0].count)
    } else {
        Err(StoreError::Database(format!(
            "count query returned {} rows",
            rows.len()
        )))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SchemaMigrationAction {
    Current,
    MigrateFrom(i64),
}

fn schema_migration_action(
    rows: &[CompatibilityRow],
    supported: ClusterCompatibility,
) -> Result<SchemaMigrationAction, StoreError> {
    let [meta] = rows else {
        return Err(StoreError::Migration(format!(
            "cluster compatibility metadata returned {} rows",
            rows.len()
        )));
    };
    if !supported.covers(meta.protocol_min, meta.protocol_max) {
        return Err(StoreError::Migration(
            supported.protocol_refusal(meta.protocol_min, meta.protocol_max),
        ));
    }
    match meta.schema_version {
        version if version == supported.schema_version => Ok(SchemaMigrationAction::Current),
        AUTH_SCHEMA_MIGRATION_SOURCE
        | BOOK_SCHEMA_MIGRATION_SOURCE
        | LEASE_SCHEMA_MIGRATION_SOURCE
        | PRETRANSCODE_SCHEMA_MIGRATION_SOURCE
        | MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE
        | SHARED_CACHE_SCHEMA_MIGRATION_SOURCE
        | FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE => {
            Ok(SchemaMigrationAction::MigrateFrom(meta.schema_version))
        }
        version => Err(StoreError::Migration(format!(
            "cluster schema {version} cannot migrate to voter schema {}",
            supported.schema_version
        ))),
    }
}

fn verify_compatibility_rows(
    rows: Vec<CompatibilityRow>,
    supported: ClusterCompatibility,
) -> Result<(), StoreError> {
    let [meta] = rows.as_slice() else {
        return Err(StoreError::Migration(format!(
            "cluster compatibility metadata returned {} rows",
            rows.len()
        )));
    };
    if meta.schema_version != supported.schema_version {
        return Err(StoreError::Migration(format!(
            "cluster schema {} is incompatible with voter schema {}",
            meta.schema_version, supported.schema_version
        )));
    }
    if !supported.covers(meta.protocol_min, meta.protocol_max) {
        return Err(StoreError::Migration(
            supported.protocol_refusal(meta.protocol_min, meta.protocol_max),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct AuthStoreDump {
    catalog_digest: String,
    durable_digest: String,
    cluster_meta: Vec<ClusterMetaDumpRow>,
    settings: Vec<SettingDumpRow>,
    users: Vec<UserDumpRow>,
    tokens: Vec<TokenDumpRow>,
    api_keys: Vec<ApiKeyDumpRow>,
    job_leases: Vec<JobLeaseDumpRow>,
    media_session_requests: Vec<MediaSessionRequestDumpRow>,
    media_playback_pointers: Vec<MediaPlaybackPointerDumpRow>,
    media_sessions: Vec<MediaSessionDumpRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CompatibilityRow {
    schema_version: i64,
    protocol_min: i64,
    protocol_max: i64,
}

impl From<&mut Row<'_>> for CompatibilityRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            schema_version: row.get("schema_version"),
            protocol_min: row.get("protocol_min"),
            protocol_max: row.get("protocol_max"),
        }
    }
}

struct PingRow {
    healthy: i64,
}

impl From<&mut Row<'_>> for PingRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            healthy: row.get("healthy"),
        }
    }
}

struct CountRow {
    count: i64,
}

/// One `id` column. Existence is the whole answer the sweep needs, but the
/// column still has to be decoded for the row to be built.
struct IdRow {
    #[allow(dead_code)]
    id: i64,
}

impl<'a> From<&'a mut hiqlite::Row<'_>> for IdRow {
    fn from(row: &'a mut hiqlite::Row<'_>) -> IdRow {
        IdRow { id: row.get("id") }
    }
}

struct PrometheusStoreRow {
    libraries: i64,
    users: i64,
    queued: i64,
    preparing: i64,
    ready: i64,
    failed: i64,
    queued_bytes: i64,
    preparing_bytes: i64,
    ready_bytes: i64,
    failed_bytes: i64,
    active_leases: i64,
    pinned_bytes: i64,
    outbox_pending: i64,
    outbox_ok: i64,
    outbox_failed: i64,
}

impl From<&mut Row<'_>> for PrometheusStoreRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            libraries: row.get("libraries"),
            users: row.get("users"),
            queued: row.get("queued"),
            preparing: row.get("preparing"),
            ready: row.get("ready"),
            failed: row.get("failed"),
            queued_bytes: row.get("queued_bytes"),
            preparing_bytes: row.get("preparing_bytes"),
            ready_bytes: row.get("ready_bytes"),
            failed_bytes: row.get("failed_bytes"),
            active_leases: row.get("active_leases"),
            pinned_bytes: row.get("pinned_bytes"),
            outbox_pending: row.get("outbox_pending"),
            outbox_ok: row.get("outbox_ok"),
            outbox_failed: row.get("outbox_failed"),
        }
    }
}

impl From<PrometheusStoreRow> for PrometheusStoreSnapshot {
    fn from(row: PrometheusStoreRow) -> Self {
        Self {
            libraries: row.libraries,
            users: row.users,
            offline: OfflinePackageStats {
                queued: row.queued,
                preparing: row.preparing,
                ready: row.ready,
                failed: row.failed,
                queued_bytes: row.queued_bytes,
                preparing_bytes: row.preparing_bytes,
                ready_bytes: row.ready_bytes,
                failed_bytes: row.failed_bytes,
                active_leases: row.active_leases,
                pinned_bytes: row.pinned_bytes,
            },
            watched_outbox: (row.outbox_pending, row.outbox_ok, row.outbox_failed),
        }
    }
}

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            count: row.get("count"),
        }
    }
}

struct SettingValueRow {
    value: String,
}

impl From<&mut Row<'_>> for SettingValueRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            value: row.get("value"),
        }
    }
}

struct SettingEntryRow {
    key: String,
    value: String,
}

impl From<&mut Row<'_>> for SettingEntryRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            key: row.get("key"),
            value: row.get("value"),
        }
    }
}

struct UserRow {
    id: i64,
    username: String,
    password_hash: String,
    is_admin: bool,
    created_at: i64,
}

struct TokenUserRow {
    user: UserRow,
    last_seen_at: i64,
}

impl From<&mut Row<'_>> for TokenUserRow {
    fn from(row: &mut Row<'_>) -> Self {
        let last_seen_at = row.get("last_seen_at");
        Self {
            user: UserRow::from(&mut *row),
            last_seen_at,
        }
    }
}

impl From<TokenUserRow> for User {
    fn from(row: TokenUserRow) -> Self {
        row.user.into()
    }
}

struct ActivityTimestampRow {
    last_activity_at: Option<i64>,
}

impl From<&mut Row<'_>> for ActivityTimestampRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            last_activity_at: row.get("last_activity_at"),
        }
    }
}

impl From<&mut Row<'_>> for UserRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            username: row.get("username"),
            password_hash: row.get("password_hash"),
            is_admin: row.get::<i64>("is_admin") != 0,
            created_at: row.get("created_at"),
        }
    }
}

impl From<UserRow> for User {
    fn from(row: UserRow) -> Self {
        Self {
            id: row.id,
            username: row.username,
            password_hash: row.password_hash,
            is_admin: row.is_admin,
            created_at: row.created_at,
        }
    }
}

struct ApiKeyRow {
    id: i64,
    name: String,
    key_hash: String,
    scopes: String,
    created_at: i64,
    last_used_at: Option<i64>,
    disabled: bool,
}

impl From<&mut Row<'_>> for ApiKeyRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            key_hash: row.get("key_hash"),
            scopes: row.get("scopes"),
            created_at: row.get("created_at"),
            last_used_at: row.get("last_used_at"),
            disabled: row.get::<i64>("disabled") != 0,
        }
    }
}

impl From<ApiKeyRow> for ApiKey {
    fn from(row: ApiKeyRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            key_hash: row.key_hash,
            // Corrupt scope data must fail closed, as in the SQLite backend.
            scopes: serde_json::from_str(&row.scopes).unwrap_or_default(),
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            disabled: row.disabled,
        }
    }
}

macro_rules! dump_row {
    ($name:ident { $($field:ident : $ty:ty),+ $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct $name { $( $field: $ty ),+ }

        impl From<&mut Row<'_>> for $name {
            fn from(row: &mut Row<'_>) -> Self {
                Self { $( $field: row.get(stringify!($field)) ),+ }
            }
        }
    };
}

dump_row!(ClusterMetaDumpRow {
    singleton: i64,
    schema_version: i64,
    protocol_min: i64,
    protocol_max: i64,
    migrated_at: i64,
});
dump_row!(SettingDumpRow {
    key: String,
    value: String,
    updated_at: i64,
});
dump_row!(UserDumpRow {
    id: i64,
    username: String,
    password_hash: String,
    is_admin: i64,
    created_at: i64,
});
dump_row!(TokenDumpRow {
    token_hash: String,
    user_id: i64,
    device: Option<String>,
    created_at: i64,
    last_seen_at: i64,
});
dump_row!(ApiKeyDumpRow {
    id: i64,
    name: String,
    key_hash: String,
    scopes: String,
    created_at: i64,
    last_used_at: Option<i64>,
    disabled: i64,
});
dump_row!(JobLeaseDumpRow {
    resource: String,
    owner_node_id: String,
    fence: i64,
    revision: i64,
    expires_at_ms: i64,
    updated_at_ms: i64,
});
dump_row!(MediaSessionRequestDumpRow {
    user_id: i64,
    request_id: String,
    request_fingerprint: String,
    playback_id: String,
    state: String,
    claim_expires_at_ms: i64,
    incarnation_id: String,
    owner_node_id: Option<String>,
    response_json: Option<String>,
    updated_at_ms: i64,
});
dump_row!(MediaPlaybackPointerDumpRow {
    user_id: i64,
    playback_id: String,
    current_incarnation_id: String,
    updated_at_ms: i64,
});
dump_row!(MediaSessionDumpRow {
    incarnation_id: String,
    session_id: String,
    user_id: i64,
    playback_id: String,
    request_fingerprint: String,
    owner_node_id: String,
    owner_epoch: i64,
    lease_expires_at_ms: i64,
    state: String,
    recipe_json: String,
    response_json: String,
    produced_playable_through_ms: i64,
    fetched_through_ms: i64,
    media_origin_ms: i64,
    media_sequence: i64,
    discontinuity_sequence: i64,
    updated_at_ms: i64,
});

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_STORE_OPERATION_METRICS: LazyLock<StoreOperationMetrics> =
        LazyLock::new(StoreOperationMetrics::default);

    #[tokio::test(start_paused = true)]
    async fn replaceable_write_gate_pins_window_failure_retry_and_terminal_bypass() {
        let gate = ReplaceableWriteGate::<String>::default();
        let submitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let run = |submitted: Arc<std::sync::atomic::AtomicUsize>| async move {
            submitted.fetch_add(1, Ordering::Relaxed);
            Ok::<_, StoreError>(())
        };

        assert_eq!(
            gate.run(
                "cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("leading touch"),
            ReplaceableWriteOutcome::Submitted
        );
        tokio::time::advance(CACHE_TOUCH_COMMIT_WINDOW - Duration::from_millis(1)).await;
        assert_eq!(
            gate.run(
                "cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("inside-window touch"),
            ReplaceableWriteOutcome::Suppressed
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            gate.run(
                "cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("boundary touch"),
            ReplaceableWriteOutcome::Submitted
        );
        assert_eq!(submitted.load(Ordering::Relaxed), 2);

        assert_eq!(
            gate.run(
                "slow-cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                async {
                    submitted.fetch_add(1, Ordering::Relaxed);
                    tokio::time::advance(Duration::from_secs(4)).await;
                    Ok(())
                },
            )
            .await
            .expect("slow leading touch"),
            ReplaceableWriteOutcome::Submitted
        );
        tokio::time::advance(Duration::from_millis(999)).await;
        assert_eq!(
            gate.run(
                "slow-cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("slow touch inside dispatch-anchored window"),
            ReplaceableWriteOutcome::Suppressed
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            gate.run(
                "slow-cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("slow touch at dispatch-anchored boundary"),
            ReplaceableWriteOutcome::Submitted
        );

        let failure = gate
            .run("failure".to_owned(), CACHE_TOUCH_COMMIT_WINDOW, async {
                Err(StoreError::Task("injected touch failure".to_owned()))
            })
            .await;
        assert!(failure.is_err());
        assert_eq!(
            gate.run(
                "failure".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("failed touch must retry"),
            ReplaceableWriteOutcome::Submitted
        );

        // Completion/removal never enter the replaceable gate. This models a
        // terminal mutation at the same instant as a suppressed activity fact.
        let terminal_commits = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(
            gate.run(
                "terminal-cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("leading activity before terminal mutation"),
            ReplaceableWriteOutcome::Submitted
        );
        assert_eq!(
            gate.run(
                "terminal-cache-use".to_owned(),
                CACHE_TOUCH_COMMIT_WINDOW,
                run(Arc::clone(&submitted)),
            )
            .await
            .expect("duplicate activity touch"),
            ReplaceableWriteOutcome::Suppressed
        );
        terminal_commits.fetch_add(1, Ordering::Relaxed);
        assert_eq!(terminal_commits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_equal_replaceable_writes_share_one_durable_result() {
        let gate = Arc::new(ReplaceableWriteGate::<String>::default());
        let submitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(81));
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..80 {
            let gate = Arc::clone(&gate);
            let submitted = Arc::clone(&submitted);
            let barrier = Arc::clone(&barrier);
            requests.spawn(async move {
                barrier.wait().await;
                gate.run(
                    "same-location".to_owned(),
                    Duration::from_secs(1),
                    async move {
                        submitted.fetch_add(1, Ordering::Relaxed);
                        tokio::task::yield_now().await;
                        Ok(())
                    },
                )
                .await
            });
        }
        barrier.wait().await;
        let mut admitted = 0;
        while let Some(result) = requests.join_next().await {
            if result.expect("touch task").expect("touch result")
                == ReplaceableWriteOutcome::Submitted
            {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 1);
        assert_eq!(submitted.load(Ordering::Relaxed), 1);

        let bypass_commits = (0..80).count();
        assert!(
            bypass_commits > admitted,
            "the uncoalesced load control must violate the one-commit burst budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn replaceable_write_gate_caps_hot_keys_and_reclaims_expired_identities() {
        let gate = ReplaceableWriteGate::<String>::default();
        for ordinal in 0..REPLACEABLE_WRITE_GATE_MAX_KEYS {
            let state = gate.key_state(format!("recent-{ordinal}"), CACHE_TOUCH_COMMIT_WINDOW);
            *state.lock().await = Some(tokio::time::Instant::now());
        }
        assert_eq!(
            gate.keys
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            REPLACEABLE_WRITE_GATE_MAX_KEYS
        );

        let active = gate.key_state("recent-0".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
        let same = gate.key_state("recent-0".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
        assert!(Arc::ptr_eq(&active, &same));
        drop(same);

        let overflow = gate.key_state("overflow".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
        assert!(
            !gate
                .keys
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key("overflow"),
            "a 4,097th recent identity must degrade without growing the map"
        );
        drop(overflow);

        tokio::time::advance(CACHE_TOUCH_COMMIT_WINDOW).await;
        let reclaimed = gate.key_state("reclaimed".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
        {
            let keys = gate
                .keys
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(
                keys.len(),
                2,
                "expired idle identities must be reclaimed while an active key stays mapped"
            );
            assert!(keys.contains_key("recent-0"));
            assert!(keys.contains_key("reclaimed"));
        }
        drop(active);
        drop(reclaimed);

        for ordinal in 0..(REPLACEABLE_WRITE_GATE_MAX_KEYS - 2) {
            drop(gate.key_state(format!("refill-{ordinal}"), CACHE_TOUCH_COMMIT_WINDOW));
        }
        let final_key = gate.key_state("final".to_owned(), CACHE_TOUCH_COMMIT_WINDOW);
        let keys = gate
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            keys.len(),
            1,
            "an expired key must become reclaimable after its active caller releases it"
        );
        assert!(keys.contains_key("final"));
        drop(keys);
        drop(final_key);
    }

    #[test]
    fn cache_terminal_mutations_cannot_enter_the_replaceable_touch_gate() {
        let source = include_str!("hiqlite_durable.rs");
        let method = |start: &str, end: &str| {
            source
                .split_once(start)
                .unwrap_or_else(|| panic!("missing {start}"))
                .1
                .split_once(end)
                .unwrap_or_else(|| panic!("missing {end}"))
                .0
        };
        assert!(method(
            "async fn touch_cache_claim",
            "async fn complete_cache_entry"
        )
        .contains("coalesce_cache_touch"));
        assert!(
            method("async fn touch_cache_entry", "async fn cache_by_age")
                .contains("coalesce_cache_touch")
        );
        for terminal in [
            method(
                "async fn complete_cache_entry",
                "async fn touch_cache_entry",
            ),
            method(
                "async fn invalidate_cache_entry",
                "async fn forget_cache_entry",
            ),
            method("async fn forget_cache_entry", "async fn cache_bytes"),
        ] {
            assert!(!terminal.contains("coalesce_cache_touch"));
            assert!(terminal.contains("self.execute") || terminal.contains(".txn("));
        }
    }

    #[tokio::test]
    async fn store_operation_timer_classifies_completion_error_and_cancellation() {
        let count = |class, outcome| {
            TEST_STORE_OPERATION_METRICS
                .cell(class, outcome)
                .count
                .load(Ordering::Relaxed)
        };
        let ok_before = count(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok);
        let error_before = count(
            StoreOperationClass::AuthorityRead,
            StoreOperationOutcome::Error,
        );
        let cancelled_before = count(StoreOperationClass::Write, StoreOperationOutcome::Cancelled);
        let statement_error_before =
            count(StoreOperationClass::Write, StoreOperationOutcome::Error);

        time_store_operation(
            &TEST_STORE_OPERATION_METRICS,
            StoreOperationClass::LocalRead,
            async { Ok::<_, StoreError>(()) },
            |_| true,
        )
        .await
        .expect("successful timed operation");
        let error = time_store_operation(
            &TEST_STORE_OPERATION_METRICS,
            StoreOperationClass::AuthorityRead,
            async { Err::<(), _>(StoreError::Database("injected failure".to_owned())) },
            |_| true,
        )
        .await;
        assert!(error.is_err());

        let nested = time_store_operation(
            &TEST_STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            async {
                Ok::<_, StoreError>(vec![Ok::<(), &'static str>(()), Err("constraint failure")])
            },
            |results| results.iter().all(Result::is_ok),
        )
        .await
        .expect("Hiqlite returns statement errors inside the operation result");
        assert!(nested.iter().any(Result::is_err));

        let mut cancelled = Box::pin(time_store_operation(
            &TEST_STORE_OPERATION_METRICS,
            StoreOperationClass::Write,
            std::future::pending::<Result<(), StoreError>>(),
            |_| true,
        ));
        assert!(matches!(
            futures_util::poll!(&mut cancelled),
            std::task::Poll::Pending
        ));
        drop(cancelled);

        assert_eq!(
            count(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok),
            ok_before + 1
        );
        assert_eq!(
            count(
                StoreOperationClass::AuthorityRead,
                StoreOperationOutcome::Error
            ),
            error_before + 1
        );
        assert_eq!(
            count(StoreOperationClass::Write, StoreOperationOutcome::Cancelled),
            cancelled_before + 1
        );
        assert_eq!(
            count(StoreOperationClass::Write, StoreOperationOutcome::Error),
            statement_error_before + 1
        );
    }

    #[cfg(feature = "cluster-read-cost-validation")]
    #[tokio::test]
    async fn validation_control_preserves_results_without_recording_any_outcome() {
        let metrics: &'static StoreOperationMetrics =
            Box::leak(Box::new(StoreOperationMetrics::default()));
        let total = || {
            metrics
                .cells
                .iter()
                .fold(0_u64, |sum, cell| sum + cell.count.load(Ordering::Relaxed))
        };

        time_store_operation_controlled(
            metrics,
            StoreOperationClass::LocalRead,
            async { Ok::<_, StoreError>(17) },
            |_| true,
            false,
        )
        .await
        .expect("disabled success preserves its value");
        let error = time_store_operation_controlled(
            metrics,
            StoreOperationClass::AuthorityRead,
            async { Err::<(), _>(StoreError::Database("control error".to_owned())) },
            |_| true,
            false,
        )
        .await
        .expect_err("disabled error remains an error");
        assert_eq!(error.to_string(), "database error: control error");
        let mut cancelled = Box::pin(time_store_operation_controlled(
            metrics,
            StoreOperationClass::Write,
            std::future::pending::<Result<(), StoreError>>(),
            |_| true,
            false,
        ));
        assert!(matches!(
            futures_util::poll!(&mut cancelled),
            std::task::Poll::Pending
        ));
        drop(cancelled);
        assert_eq!(total(), 0, "the control arm recorded an outcome");

        time_store_operation_controlled(
            metrics,
            StoreOperationClass::LocalRead,
            async { Ok::<_, StoreError>(17) },
            |_| true,
            true,
        )
        .await
        .expect("instrumented success");
        let _ = time_store_operation_controlled(
            metrics,
            StoreOperationClass::AuthorityRead,
            async { Err::<(), _>(StoreError::Database("measured error".to_owned())) },
            |_| true,
            true,
        )
        .await;
        let mut cancelled = Box::pin(time_store_operation_controlled(
            metrics,
            StoreOperationClass::Write,
            std::future::pending::<Result<(), StoreError>>(),
            |_| true,
            true,
        ));
        assert!(matches!(
            futures_util::poll!(&mut cancelled),
            std::task::Poll::Pending
        ));
        drop(cancelled);
        assert_eq!(total(), 3, "the measured arm omitted an outcome class");
    }

    #[tokio::test(start_paused = true)]
    async fn authority_reads_retry_one_transient_failure_and_nothing_else() {
        let metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let retried = time_authority_read_with_retry(metrics, {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                async move {
                    if attempt == 0 {
                        Err(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))
                    } else {
                        Ok(42)
                    }
                }
            }
        })
        .await
        .expect("the bounded retry recovers");
        assert_eq!(retried, 42);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        assert_eq!(
            metrics
                .cell(
                    StoreOperationClass::AuthorityRead,
                    StoreOperationOutcome::Error,
                )
                .count
                .load(Ordering::Relaxed),
            1,
            "the timed-out attempt remains visible in metrics"
        );
        assert_eq!(
            metrics
                .cell(
                    StoreOperationClass::AuthorityRead,
                    StoreOperationOutcome::Ok,
                )
                .count
                .load(Ordering::Relaxed),
            1
        );

        let quorum_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recovered = time_authority_read_with_retry(metrics, {
            let attempts = Arc::clone(&quorum_attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                async move {
                    if attempt == 0 {
                        Err(StoreError::Database(
                            "CheckIsLeaderError: not enough for a quorum; got:{1}".to_owned(),
                        ))
                    } else {
                        Ok(84)
                    }
                }
            }
        })
        .await
        .expect("a transient quorum check gets the same bounded retry");
        assert_eq!(recovered, 84);
        assert_eq!(quorum_attempts.load(Ordering::Relaxed), 2);

        let permanent_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let error = time_authority_read_with_retry(metrics, {
            let attempts = Arc::clone(&permanent_attempts);
            move || {
                attempts.fetch_add(1, Ordering::Relaxed);
                async { Err::<(), _>(StoreError::Database("bad row".to_owned())) }
            }
        })
        .await
        .expect_err("non-timeout database errors are terminal");
        assert_eq!(error.to_string(), "database error: bad row");
        assert_eq!(permanent_attempts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn idempotent_writes_absorb_two_election_deadlines_and_nothing_else() {
        let metrics = Box::leak(Box::new(StoreOperationMetrics::default()));
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let value = time_idempotent_write_with_retry(metrics, {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                async move {
                    if attempt < 2 {
                        Err(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned()))
                    } else {
                        Ok(42)
                    }
                }
            }
        })
        .await
        .expect("the third exact-state attempt recovers after election");
        assert_eq!(value, 42);
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(
            metrics
                .cell(StoreOperationClass::Write, StoreOperationOutcome::Error)
                .count
                .load(Ordering::Relaxed),
            2,
            "each timed-out physical attempt remains visible"
        );
        assert_eq!(
            metrics
                .cell(StoreOperationClass::Write, StoreOperationOutcome::Ok)
                .count
                .load(Ordering::Relaxed),
            1
        );

        let permanent_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let error = time_idempotent_write_with_retry(metrics, {
            let attempts = Arc::clone(&permanent_attempts);
            move || {
                attempts.fetch_add(1, Ordering::Relaxed);
                async { Err::<(), _>(StoreError::Database("constraint".to_owned())) }
            }
        })
        .await
        .expect_err("non-timeout write errors are terminal");
        assert_eq!(error.to_string(), "database error: constraint");
        assert_eq!(permanent_attempts.load(Ordering::Relaxed), 1);

        let exhausted_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let error = time_idempotent_write_with_retry(metrics, {
            let attempts = Arc::clone(&exhausted_attempts);
            move || {
                attempts.fetch_add(1, Ordering::Relaxed);
                async { Err::<(), _>(StoreError::Database(REPLICATED_STORE_TIMEOUT.to_owned())) }
            }
        })
        .await
        .expect_err("the exact-state retry budget remains bounded");
        assert_eq!(
            error.to_string(),
            format!("database error: {REPLICATED_STORE_TIMEOUT}")
        );
        assert_eq!(
            exhausted_attempts.load(Ordering::Relaxed),
            IDEMPOTENT_WRITE_MAX_ATTEMPTS
        );
    }

    #[test]
    fn store_operation_histogram_pins_boundaries_and_saturates() {
        let metrics = StoreOperationMetrics::default();
        metrics.record(
            StoreOperationClass::LocalRead,
            StoreOperationOutcome::Ok,
            Duration::from_nanos(1_000_000),
        );
        metrics.record(
            StoreOperationClass::LocalRead,
            StoreOperationOutcome::Ok,
            Duration::from_nanos(1_000_001),
        );
        metrics.record(
            StoreOperationClass::LocalRead,
            StoreOperationOutcome::Ok,
            Duration::from_secs(11),
        );
        let cell = metrics.cell(StoreOperationClass::LocalRead, StoreOperationOutcome::Ok);
        assert_eq!(cell.count.load(Ordering::Relaxed), 3);
        assert_eq!(cell.elapsed_nanos.load(Ordering::Relaxed), 11_002_000_001);
        assert_eq!(cell.buckets[0].load(Ordering::Relaxed), 1);
        assert_eq!(cell.buckets[1].load(Ordering::Relaxed), 1);
        assert_eq!(cell.buckets[2].load(Ordering::Relaxed), 0);

        let exposition = metrics.render();
        assert!(exposition.contains(
            "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"0.001\"} 1"
        ));
        assert!(exposition.contains(
            "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"0.005\"} 2"
        ));
        assert!(exposition.contains(
            "plurx_store_operation_seconds_bucket{class=\"local_read\",outcome=\"ok\",le=\"+Inf\"} 3"
        ));

        let saturated = metrics.cell(
            StoreOperationClass::AuthorityRead,
            StoreOperationOutcome::Error,
        );
        saturated.count.store(u64::MAX, Ordering::Relaxed);
        saturated
            .elapsed_nanos
            .store(u64::MAX - 1, Ordering::Relaxed);
        saturated.buckets[0].store(u64::MAX, Ordering::Relaxed);
        metrics.record(
            StoreOperationClass::AuthorityRead,
            StoreOperationOutcome::Error,
            Duration::from_nanos(10),
        );
        assert_eq!(saturated.count.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(saturated.elapsed_nanos.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(saturated.buckets[0].load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn store_operation_exposition_has_only_the_fixed_label_matrix() {
        let exposition = TEST_STORE_OPERATION_METRICS.render();
        assert!(exposition.contains("# TYPE plurx_store_operation_seconds histogram"));
        assert!(exposition.contains("# TYPE plurx_store_operations_total counter"));
        assert!(!exposition.contains("sql="));
        assert!(!exposition.contains("route="));
        assert_eq!(
            exposition
                .lines()
                .filter(|line| line.starts_with("plurx_store_operations_total{"))
                .count(),
            9
        );
        assert_eq!(
            exposition
                .lines()
                .filter(|line| line.starts_with("plurx_store_operation_seconds_count{"))
                .count(),
            9
        );
        assert_eq!(
            exposition
                .lines()
                .filter(|line| line.starts_with("plurx_store_operation_seconds_bucket{"))
                .count(),
            9 * (STORE_OPERATION_BUCKETS.len() + 1)
        );
        for class in ["local_read", "authority_read", "write"] {
            for outcome in ["ok", "error", "cancelled"] {
                assert!(exposition.contains(&format!(
                    "plurx_store_operations_total{{class=\"{class}\",outcome=\"{outcome}\"}}"
                )));
            }
        }
    }

    #[tokio::test]
    async fn failed_activity_operations_release_both_credential_reservations_for_retry() {
        for credential in [
            ActivityCredential::Token("token-hash".to_owned()),
            ActivityCredential::ApiKey(42),
        ] {
            let gate = Arc::new(ActivityRefreshGate::default());
            let failure = gate
                .run(credential.clone(), 1_000, async {
                    Err(StoreError::Task(
                        "injected activity write failure".to_owned(),
                    ))
                })
                .await;
            assert!(failure.is_err());

            let retried = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let retried_inside = Arc::clone(&retried);
            gate.run(credential.clone(), 1_000, async move {
                retried_inside.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("the same credential retries after its failed operation");
            assert!(retried.load(std::sync::atomic::Ordering::Relaxed));

            let suppressed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let suppressed_inside = Arc::clone(&suppressed);
            gate.run(credential.clone(), 1_060, async move {
                suppressed_inside.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
            .expect("a retained reservation suppresses the exact boundary");
            assert!(!suppressed.load(std::sync::atomic::Ordering::Relaxed));
            assert!(gate.try_reserve(credential, 1_061).is_some());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn three_independent_activity_gates_each_admit_one_concurrent_operation() {
        let gates: [Arc<ActivityRefreshGate>; 3] =
            std::array::from_fn(|_| Arc::new(ActivityRefreshGate::default()));
        let admitted: [Arc<std::sync::atomic::AtomicUsize>; 3] =
            std::array::from_fn(|_| Arc::new(std::sync::atomic::AtomicUsize::new(0)));
        let barrier = Arc::new(tokio::sync::Barrier::new(121));
        let mut requests = tokio::task::JoinSet::new();
        for ordinal in 0..120 {
            let gate_index = ordinal % gates.len();
            let gate = Arc::clone(&gates[gate_index]);
            let admitted = Arc::clone(&admitted[gate_index]);
            let barrier = Arc::clone(&barrier);
            requests.spawn(async move {
                barrier.wait().await;
                gate.run(
                    ActivityCredential::Token("shared-token-hash".to_owned()),
                    1_000,
                    async move {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        Ok(())
                    },
                )
                .await
            });
        }
        barrier.wait().await;
        while let Some(result) = requests.join_next().await {
            result
                .expect("join gate request")
                .expect("run gate operation");
        }
        assert_eq!(
            admitted.map(|value| value.load(std::sync::atomic::Ordering::Relaxed)),
            [1, 1, 1]
        );
    }

    #[test]
    fn replicated_store_modules_cannot_bypass_the_timed_client_accessor() {
        for (name, source) in [
            ("catalog", include_str!("hiqlite_catalog.rs")),
            ("coordination", include_str!("hiqlite_coordination.rs")),
            ("media", include_str!("hiqlite_media.rs")),
            ("durable", include_str!("hiqlite_durable.rs")),
            ("import", include_str!("hiqlite_import.rs")),
            ("pretranscode", include_str!("hiqlite_pretranscode.rs")),
            ("publication", include_str!("hiqlite_publication.rs")),
            ("reading", include_str!("hiqlite_reading.rs")),
        ] {
            let compact: String = source
                .chars()
                .filter(|char| !char.is_whitespace())
                .collect();
            assert!(
                !compact.contains("self.client."),
                "{name} store bypasses HiqliteAuthStore::client(), which enforces the 3s timeout"
            );
        }
        let own_source = include_str!("hiqlite.rs")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let forbidden_nested_timeout = ["timeout_store(", "self.client()"].concat();
        assert!(
            !own_source.contains(&forbidden_nested_timeout),
            "TimedClient owns the only operation timeout; an equal outer timeout races its terminal metric classification"
        );
    }

    #[tokio::test]
    async fn timed_read_accessors_reject_misordered_placeholders_before_io() {
        let client = disconnected_test_client();
        for error in [
            client
                .query_consistent_map::<CountRow, _>(
                    "SELECT $2 AS count WHERE $1 = 1",
                    params!(1, 2),
                )
                .await
                .err()
                .expect("consistent helper must validate SQL"),
            client
                .query_map::<CountRow, _>("SELECT $2 AS count WHERE $1 = 1", params!(1, 2))
                .await
                .err()
                .expect("local helper must validate SQL"),
        ] {
            assert!(error.to_string().contains("expected $1, found $2"));
        }
    }

    #[test]
    fn replicated_auth_schema_and_writes_bind_every_clock_value() {
        validate_sql(AUTH_SCHEMA).expect("schema has no connection-local values");
        for sql in [
            "INSERT INTO settings VALUES ($1, $2, $3)",
            "INSERT INTO users VALUES ($1, $2, $3, $4, $5)",
            "INSERT INTO tokens VALUES ($1, $2, $3, $4, $4)",
            "INSERT INTO api_keys VALUES ($1, $2, $3, $4, $5, $6, $7)",
        ] {
            validate_sql(sql).expect("bound replicated write");
        }

        let error = validate_sql("UPDATE users SET password_hash = $2 WHERE id = $1")
            .expect_err("out-of-order named parameters");
        assert!(error.to_string().contains("expected $1, found $2"));
        validate_sql("UPDATE users SET password_hash = $1 WHERE id = $2")
            .expect("first-appearance order");
        validate_sql("SELECT '$2', id FROM users WHERE id = $1 -- $3")
            .expect("quoted and commented placeholders are data");
        validate_sql("SELECT [$2], [a'b] FROM users WHERE id = $1")
            .expect("bracketed identifiers are not placeholders or strings");

        for unsupported in [
            "SELECT $1, $01, $2",
            "SELECT ?2, ?1",
            "SELECT ? FROM users",
            "SELECT :id FROM users",
            "SELECT @id FROM users",
            "SELECT $id FROM users",
        ] {
            validate_sql(unsupported).expect_err("unsupported placeholder spelling");
        }
        validate_sql("SELECT [a'b], $2, $1 FROM users")
            .expect_err("bracket quote cannot hide misordered placeholders");
    }

    /// The binary that shipped before P6: it implements exactly protocol 4.
    const PREVIOUS_RELEASE: ClusterCompatibility = ClusterCompatibility {
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_min: AUTH_PROTOCOL_VERSION,
        protocol_max: AUTH_PROTOCOL_VERSION,
    };

    fn meta(protocol_min: i64, protocol_max: i64) -> CompatibilityRow {
        CompatibilityRow {
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_min,
            protocol_max,
        }
    }

    #[test]
    fn compatibility_rejects_schema_and_protocol_drift() {
        verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
            ClusterCompatibility::CURRENT,
        )
        .expect("current voter");

        let old_schema = ClusterCompatibility {
            schema_version: AUTH_SCHEMA_VERSION - 1,
            ..ClusterCompatibility::CURRENT
        };
        let error =
            verify_compatibility_rows(vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)], old_schema)
                .expect_err("old schema must refuse");
        assert!(error.to_string().contains("incompatible"));

        let existing_v4_schema = CompatibilityRow {
            schema_version: 4,
            protocol_min: AUTH_PROTOCOL_MIN,
            protocol_max: AUTH_PROTOCOL_MIN,
        };
        let error =
            verify_compatibility_rows(vec![existing_v4_schema], ClusterCompatibility::CURRENT)
                .expect_err("the strict open path must never migrate");
        assert!(error.to_string().contains("schema 4 is incompatible"));

        // A cluster that has moved past every protocol this binary knows.
        let error = verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MIN - 1, AUTH_PROTOCOL_MIN - 1)],
            ClusterCompatibility::CURRENT,
        )
        .expect_err("a dropped protocol must refuse");
        assert!(error.to_string().contains("too new"), "{error}");
    }

    /// The four P6 transitions, in one place, in the order an operator meets
    /// them. Each assertion is a supported-or-refused verdict for a real
    /// deployment step, not a field copy.
    #[test]
    fn protocol_range_governs_every_upgrade_and_activation_transition() {
        // 1. Existing cluster on 4..=4, new binary. This is the whole upgrade
        //    path: installing the range-aware build must need no operator
        //    action and must not change anything.
        verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
            ClusterCompatibility::CURRENT,
        )
        .expect("a 4..=4 cluster admits the [4,5] binary unchanged");

        // 2. Existing cluster on 4..=4, previous release. Rolling upgrades mean
        //    both binaries run against the same unchanged cluster range.
        verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
            PREVIOUS_RELEASE,
        )
        .expect("a 4..=4 cluster still admits the previous release");

        // 3. Activated cluster on 5..=5, new binary.
        verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MAX)],
            ClusterCompatibility::CURRENT,
        )
        .expect("an activated cluster admits the [4,5] binary");

        // 4. Activated cluster on 5..=5, previous release. This is the refusal
        //    that keeps an old voter from rejoining a learner-bearing cluster
        //    and reinterpreting its membership.
        let error = verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MAX)],
            PREVIOUS_RELEASE,
        )
        .expect_err("an activated cluster must refuse the previous release");
        let message = error.to_string();
        assert!(message.contains("protocol 5"), "{message}");
        assert!(message.contains("too old"), "{message}");
        assert!(message.contains("4..=4"), "{message}");

        // Overlap is not enough: a cluster that still needs 4 while also using
        // 5 is covered only by a binary that implements both.
        assert!(ClusterCompatibility::CURRENT.covers(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MAX));
        assert!(!PREVIOUS_RELEASE.covers(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MAX));
    }

    /// Widening `protocol_max` is the one mistake that silently strands the
    /// rest of the fleet, so pin that bootstrap picks the floor of the
    /// binary's range and that the range is written exactly once.
    #[test]
    fn bootstrap_writes_the_unactivated_floor_and_never_widens_it() {
        let production = include_str!("hiqlite.rs")
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("test module boundary")
            .0;
        let bootstrap = production
            .split_once("INSERT INTO cluster_meta")
            .expect("bootstrap insert")
            .1
            .split_once(".await?;")
            .expect("end of the bootstrap statement")
            .0;
        assert!(
            bootstrap.contains("VALUES (1, $1, $2, $2, $3)"),
            "bootstrap must write one protocol value into both range columns"
        );
        assert!(
            bootstrap.contains("AUTH_PROTOCOL_MIN"),
            "bootstrap must seed the range floor, never the activated maximum"
        );
        // Every `cluster_meta` write in the store layer belongs to the schema
        // migration chain and touches only `schema_version`/`migrated_at`. The
        // protocol range is moved by an explicit membership operation, never as
        // a side effect of opening or migrating a store.
        assert_eq!(
            production.matches("UPDATE cluster_meta").count(),
            production
                .matches("UPDATE cluster_meta SET schema_version = $1, migrated_at = $2")
                .count(),
            "a cluster_meta write in the store layer touched something other than \
             the schema version"
        );
        // A freshly bootstrapped cluster is joinable by the previous release.
        verify_compatibility_rows(
            vec![meta(AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)],
            PREVIOUS_RELEASE,
        )
        .expect("a fresh cluster stays on the unactivated range");
    }

    #[test]
    fn daemon_schema_gate_accepts_the_complete_supported_chain() {
        assert_eq!(
            AUTH_SCHEMA_MIGRATION_SOURCE + 7,
            AUTH_SCHEMA_VERSION,
            "this implementation contains every additive v5→v12 step"
        );
        let row = |schema_version| CompatibilityRow {
            schema_version,
            protocol_min: AUTH_PROTOCOL_VERSION,
            protocol_max: AUTH_PROTOCOL_VERSION,
        };
        assert_eq!(
            schema_migration_action(&[row(AUTH_SCHEMA_VERSION)], ClusterCompatibility::CURRENT)
                .expect("current schema"),
            SchemaMigrationAction::Current
        );
        assert_eq!(
            schema_migration_action(
                &[row(AUTH_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("oldest supported predecessor"),
            SchemaMigrationAction::MigrateFrom(AUTH_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(BOOK_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("book-schema predecessor"),
            SchemaMigrationAction::MigrateFrom(BOOK_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(LEASE_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("lease-schema predecessor"),
            SchemaMigrationAction::MigrateFrom(LEASE_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("immediate predecessor"),
            SchemaMigrationAction::MigrateFrom(PRETRANSCODE_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("media-session predecessor"),
            SchemaMigrationAction::MigrateFrom(MEDIA_SESSION_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("shared-cache predecessor"),
            SchemaMigrationAction::MigrateFrom(SHARED_CACHE_SCHEMA_MIGRATION_SOURCE)
        );
        assert_eq!(
            schema_migration_action(
                &[row(FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE)],
                ClusterCompatibility::CURRENT,
            )
            .expect("fragment-index predecessor"),
            SchemaMigrationAction::MigrateFrom(FRAGMENT_INDEX_SCHEMA_MIGRATION_SOURCE)
        );

        for rows in [Vec::new(), vec![row(4)], vec![row(7), row(7)]] {
            let error = schema_migration_action(&rows, ClusterCompatibility::CURRENT)
                .expect_err("ambiguous or unsupported state must fail before writes");
            assert!(
                error.to_string().contains("cannot migrate")
                    || error.to_string().contains("metadata returned"),
                "{error}"
            );
        }

        // A cluster whose active protocol this binary does not implement must
        // be refused before the migration chain runs, not after.
        let incompatible_protocol = CompatibilityRow {
            schema_version: AUTH_SCHEMA_MIGRATION_SOURCE,
            protocol_min: AUTH_PROTOCOL_MAX + 1,
            protocol_max: AUTH_PROTOCOL_MAX + 1,
        };
        let error =
            schema_migration_action(&[incompatible_protocol], ClusterCompatibility::CURRENT)
                .expect_err("protocol drift must fail before migration");
        assert!(error.to_string().contains("too old"), "{error}");

        // The activated range is inside this binary's range, so the migration
        // chain still runs against an activated cluster.
        assert_eq!(
            schema_migration_action(
                &[CompatibilityRow {
                    schema_version: AUTH_SCHEMA_MIGRATION_SOURCE,
                    protocol_min: AUTH_PROTOCOL_MAX,
                    protocol_max: AUTH_PROTOCOL_MAX,
                }],
                ClusterCompatibility::CURRENT,
            )
            .expect("an activated cluster may still be migrated by this binary"),
            SchemaMigrationAction::MigrateFrom(AUTH_SCHEMA_MIGRATION_SOURCE)
        );
    }
}
