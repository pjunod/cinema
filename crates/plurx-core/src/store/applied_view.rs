//! Node-local views of the applied Raft log (playback-start E1,
//! `docs/streaming/PLAYBACK-START-E1-BUILD.md` §4.2).
//!
//! An [`AppliedView`] answers a read from this node's own applied replica when
//! it can prove that replica is current, and from the existing [`Store`]
//! method — a quorum read on a replicated node — when it cannot. One rule for
//! every read, with `P` the serving proof:
//!
//! 1. Load the image generation `g0` and the table's revision `r0`. An odd
//!    generation, no proof, or an unmet floor answers from Authority.
//! 2. Run the local statement.
//! 3. Require the generation unchanged and the proof still valid; otherwise
//!    discard the result and answer from Authority. A local SQL error answers
//!    from Authority.
//! 4. A result is stamped `(g0, r0)` — the values loaded *before* the read —
//!    and stays current only while both still match and step 1 passes.
//!
//! "Authority" is the existing `Store` method under the caller's deadline. No
//! wait, no retry, no timer: the only time bound is the proof's own lease.
//!
//! **The floor.** A lagging replica with a valid proof can be older than a
//! fact this process already holds (its own acknowledged write, or a quorum
//! read it just made). A fact learned at instant `t` closes the view for its
//! table until a quorum confirmation whose attempt started after `t` has been
//! applied here ([`PassiveRaftMetrics::confirmed_since`]).
//!
//! **Standalone.** On the SQLite backend the store is the only copy: reads
//! forward to it and every proof and floor is met. A replicated process with
//! no local replica (a remote-authority client) answers every read from
//! Authority and never meets a floor. Neither hands out stamps.
//!
//! E1a builds the view and its counters; no call site reads through it yet.

use super::{Store, StoreError};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "hiqlite-store")]
use super::HiqliteAuthStore;
#[cfg(feature = "hiqlite-store")]
use crate::cluster::migration::status::{PassiveRaftMetrics, ServingProof};

/// Tables this node's applied mark counts, one revision each. Production
/// registers exactly these (`production_hiqlite_defaults_with_read_pool`).
pub const APPLIED_VIEW_TABLES: &[&str] = &["settings", "media_sessions"];

/// One applied view, by the table it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppliedTable {
    Settings,
    MediaSessions,
}

impl AppliedTable {
    pub const ALL: [Self; 2] = [Self::Settings, Self::MediaSessions];

    /// The replicated table behind this view.
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::MediaSessions => "media_sessions",
        }
    }

    /// The `view` label of `plurx_applied_view_*`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::MediaSessions => "route",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Settings => 0,
            Self::MediaSessions => 1,
        }
    }
}

/// Where an answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewSource {
    /// This node's applied replica (or, on SQLite, the only copy).
    View,
    /// The existing `Store` method: a quorum read on a replicated node.
    Authority,
}

/// Why a read was answered from Authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewFallback {
    /// No serving proof: lag, a leader gap, a protocol mismatch, or no local
    /// replica at all.
    NoProof,
    /// The image is being replaced, was replaced during the read, or a
    /// replacement failed.
    ImageReplacing,
    /// This process holds a fact newer than anything the replica is proven to
    /// have applied.
    Floor,
    /// The proof lapsed while the local statement ran.
    RevokedDuringRead,
    /// The session table is still in its legacy layout (routes only).
    LegacyLayout,
    /// The local statement failed.
    LocalError,
    /// The applied mark has no slot for this table.
    Unregistered,
}

impl ViewFallback {
    const ALL: [Self; 7] = [
        Self::NoProof,
        Self::ImageReplacing,
        Self::Floor,
        Self::RevokedDuringRead,
        Self::LegacyLayout,
        Self::LocalError,
        Self::Unregistered,
    ];

    /// The `reason` label of `plurx_applied_view_reads_total`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoProof => "no_proof",
            Self::ImageReplacing => "image_replacing",
            Self::Floor => "floor",
            Self::RevokedDuringRead => "revoked_during_read",
            Self::LegacyLayout => "legacy_layout",
            Self::LocalError => "local_error",
            Self::Unregistered => "unregistered",
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::NoProof => 1,
            Self::ImageReplacing => 2,
            Self::Floor => 3,
            Self::RevokedDuringRead => 4,
            Self::LegacyLayout => 5,
            Self::LocalError => 6,
            Self::Unregistered => 7,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == code)
    }
}

/// What a cached entry made from a view answer is stamped with: the image
/// generation and table revision loaded before the read. Process-local; it
/// never crosses the wire and means nothing on another node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "hiqlite-store"), allow(dead_code))]
pub struct ViewStamp {
    table: AppliedTable,
    generation: u64,
    revision: u64,
}

/// A digest of requested settings: the requested keys in key order, each with
/// its value or an absence marker. The same committed values give the same
/// digest on every node and across restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettingsDigest(pub [u8; 32]);

impl SettingsDigest {
    #[must_use]
    pub fn of(keys: &[&str], values: &BTreeMap<String, String>) -> Self {
        use sha2::{Digest, Sha256};
        let mut ordered: Vec<&str> = keys.to_vec();
        ordered.sort_unstable();
        ordered.dedup();
        let mut hasher = Sha256::new();
        for key in ordered {
            hasher.update((key.len() as u64).to_be_bytes());
            hasher.update(key.as_bytes());
            match values.get(key) {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update((value.len() as u64).to_be_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }
        Self(hasher.finalize().into())
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// [`AppliedView::settings`]: the requested values, their digest, and where
/// they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedSettings {
    pub values: BTreeMap<String, String>,
    pub digest: SettingsDigest,
    pub source: ViewSource,
    pub stamp: Option<ViewStamp>,
}

/// Advisory facts for Settings → Developer and `/metrics`, loaded at read
/// time. Carries no authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedViewObservation {
    /// `replicated`, `standalone` or `remote_authority`.
    pub mode: &'static str,
    pub proof_present: bool,
    /// `None` where there is no mark (standalone, remote authority).
    pub generation: Option<u64>,
    pub applied_index: Option<u64>,
    pub failed_replacements: u64,
    /// `(view label, revision)` per registered view table.
    pub revisions: Vec<(&'static str, u64)>,
    pub last_fallback: Option<ViewFallback>,
    /// `(view, source, reason, count)` for every combination.
    pub reads: Vec<(&'static str, &'static str, &'static str, u64)>,
    /// `(view, count)`: memo or cache reloads a stamp mismatch caused.
    pub reloads: Vec<(&'static str, u64)>,
}

impl AppliedViewObservation {
    #[must_use]
    pub fn generation_stable(&self) -> Option<bool> {
        self.generation.map(|generation| generation & 1 == 0)
    }

    #[must_use]
    pub fn total(&self, source: ViewSource) -> u64 {
        let label = source_label(source);
        self.reads
            .iter()
            .filter(|(_, read_source, _, _)| *read_source == label)
            .map(|(_, _, _, count)| *count)
            .sum()
    }
}

const fn source_label(source: ViewSource) -> &'static str {
    match source {
        ViewSource::View => "view",
        ViewSource::Authority => "authority",
    }
}

/// `[view][outcome]`, outcome 0 = answered by the view, else a fallback code.
const OUTCOMES: usize = 8;

#[derive(Default)]
struct ViewCounters {
    reads: [[AtomicU64; OUTCOMES]; 2],
    reloads: [AtomicU64; 2],
    last_fallback: AtomicU8,
}

impl ViewCounters {
    fn view(&self, table: AppliedTable) {
        self.reads[table.index()][0].fetch_add(1, Ordering::Relaxed);
    }

    fn authority(&self, table: AppliedTable, reason: ViewFallback) {
        self.reads[table.index()][usize::from(reason.code())].fetch_add(1, Ordering::Relaxed);
        self.last_fallback.store(reason.code(), Ordering::Relaxed);
    }
}

/// Per-table floors: the latest monotonic instant at which this process
/// learned a fact the replica must contain before the view may answer.
struct TableFloors {
    base: Instant,
    /// Nanoseconds after `base`, plus one; zero means no floor.
    at: [AtomicU64; 2],
}

impl TableFloors {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            at: Default::default(),
        }
    }

    fn raise(&self, table: AppliedTable, at: Instant) {
        let nanos = u64::try_from(at.saturating_duration_since(self.base).as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1);
        self.at[table.index()].fetch_max(nanos, Ordering::AcqRel);
    }

    fn get(&self, table: AppliedTable) -> Option<Instant> {
        match self.at[table.index()].load(Ordering::Acquire) {
            0 => None,
            nanos => Some(self.base + std::time::Duration::from_nanos(nanos - 1)),
        }
    }
}

#[cfg(feature = "hiqlite-store")]
struct Replicated {
    store: Arc<HiqliteAuthStore>,
    metrics: PassiveRaftMetrics,
    mark: hiqlite::AppliedMarkHandle,
    slots: [Option<hiqlite::AppliedSlot>; 2],
    #[cfg(feature = "cluster-read-cost-validation")]
    revoke_after_next_local: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "hiqlite-store")]
struct Admission {
    proof: ServingProof,
    stamp: ViewStamp,
}

#[cfg(feature = "hiqlite-store")]
impl Replicated {
    fn slot(&self, table: AppliedTable) -> Result<hiqlite::AppliedSlot, ViewFallback> {
        self.slots[table.index()].ok_or(ViewFallback::Unregistered)
    }

    fn floor_met(&self, at: Instant) -> bool {
        self.metrics
            .confirmed_since(at)
            .is_some_and(|confirmed| confirmed <= self.mark.applied_index())
    }

    /// Step 1.
    fn admit(
        &self,
        table: AppliedTable,
        floor: Option<Instant>,
    ) -> Result<Admission, ViewFallback> {
        let slot = self.slot(table)?;
        let generation = self.mark.generation();
        if generation & 1 != 0 {
            return Err(ViewFallback::ImageReplacing);
        }
        let revision = self.mark.revision(slot);
        let proof = self
            .metrics
            .eligible_serving_proof()
            .ok_or(ViewFallback::NoProof)?;
        if floor.is_some_and(|at| !self.floor_met(at)) {
            return Err(ViewFallback::Floor);
        }
        Ok(Admission {
            proof,
            stamp: ViewStamp {
                table,
                generation,
                revision,
            },
        })
    }

    /// Steps 1-3 around one local statement.
    async fn read<T, F, Fut>(
        &self,
        table: AppliedTable,
        floor: Option<Instant>,
        local: F,
    ) -> Result<(T, ViewStamp), ViewFallback>
    where
        F: FnOnce(Arc<HiqliteAuthStore>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        let admission = self.admit(table, floor)?;
        let result = local(Arc::clone(&self.store)).await;
        #[cfg(feature = "cluster-read-cost-validation")]
        if self.revoke_after_next_local.swap(false, Ordering::Relaxed) {
            self.metrics.validation_revoke_bounded_proof();
        }
        let value = result.map_err(|_| ViewFallback::LocalError)?;
        if self.mark.generation() != admission.stamp.generation {
            return Err(ViewFallback::ImageReplacing);
        }
        if !self.metrics.serving_proof_remains_valid(&admission.proof) {
            return Err(ViewFallback::RevokedDuringRead);
        }
        Ok((value, admission.stamp))
    }

    /// Step 4.
    fn check(&self, stamp: &ViewStamp, floor: Option<Instant>) -> StampCheck {
        let slot = match self.slot(stamp.table) {
            Ok(slot) => slot,
            Err(reason) => return StampCheck::Closed(reason),
        };
        let generation = self.mark.generation();
        if generation & 1 != 0 {
            return StampCheck::Closed(ViewFallback::ImageReplacing);
        }
        if self.metrics.eligible_serving_proof().is_none() {
            return StampCheck::Closed(ViewFallback::NoProof);
        }
        if floor.is_some_and(|at| !self.floor_met(at)) {
            return StampCheck::Closed(ViewFallback::Floor);
        }
        if generation != stamp.generation || self.mark.revision(slot) != stamp.revision {
            return StampCheck::Stale;
        }
        StampCheck::Current
    }
}

/// Whether an entry stamped by an earlier view read may still answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampCheck {
    /// Same image, same table revision, proof and floor still met.
    Current,
    /// The table or the image moved since: re-read through the view.
    Stale,
    /// The view is closed for this read: answer from Authority.
    Closed(ViewFallback),
}

#[derive(Clone)]
enum Mode {
    Standalone,
    RemoteAuthority,
    #[cfg(feature = "hiqlite-store")]
    Replicated(Arc<Replicated>),
}

/// The node-local applied view of `settings` and `media_sessions`.
#[derive(Clone)]
pub struct AppliedView {
    authority: Arc<dyn Store>,
    mode: Mode,
    floors: Arc<TableFloors>,
    counters: Arc<ViewCounters>,
}

impl AppliedView {
    /// SQLite (and SQLite recovery): the store is the only copy, so every
    /// read forwards to it and every proof and floor is met.
    #[must_use]
    pub fn authority(store: Arc<dyn Store>) -> Self {
        Self::with_mode(store, Mode::Standalone)
    }

    /// A replicated process without a local replica: every read is a quorum
    /// read and no floor is ever met.
    #[must_use]
    pub fn remote_authority(store: Arc<dyn Store>) -> Self {
        Self::with_mode(store, Mode::RemoteAuthority)
    }

    fn with_mode(authority: Arc<dyn Store>, mode: Mode) -> Self {
        Self {
            authority,
            mode,
            floors: Arc::new(TableFloors::new()),
            counters: Arc::new(ViewCounters::default()),
        }
    }

    /// An embedded voter or learner. A client that is not local has no mark
    /// and becomes [`Self::remote_authority`].
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn replicated(
        authority: Arc<dyn Store>,
        store: Arc<HiqliteAuthStore>,
        metrics: PassiveRaftMetrics,
        client: &hiqlite::Client,
    ) -> Self {
        match client.local_db_applied_mark() {
            Ok(mark) => Self::replicated_with_mark(authority, store, metrics, mark),
            Err(_) => Self::remote_authority(authority),
        }
    }

    #[cfg(feature = "hiqlite-store")]
    fn replicated_with_mark(
        authority: Arc<dyn Store>,
        store: Arc<HiqliteAuthStore>,
        metrics: PassiveRaftMetrics,
        mark: hiqlite::AppliedMarkHandle,
    ) -> Self {
        let slots = AppliedTable::ALL.map(|table| mark.slot(table.table()));
        if slots.iter().any(Option::is_none) {
            tracing::error!(
                registered = ?mark.observation().tables,
                "the applied mark does not register every applied-view table; those views \
                 answer from Authority"
            );
        }
        Self::with_mode(
            authority,
            Mode::Replicated(Arc::new(Replicated {
                store,
                metrics,
                mark,
                slots,
                #[cfg(feature = "cluster-read-cost-validation")]
                revoke_after_next_local: std::sync::atomic::AtomicBool::new(false),
            })),
        )
    }

    /// The production replicated view over an explicit proof source, for
    /// seeded-store contracts outside this crate.
    #[cfg(all(feature = "hiqlite-store", feature = "cluster-read-cost-validation"))]
    #[doc(hidden)]
    #[must_use]
    pub fn validation_replicated(
        authority: Arc<dyn Store>,
        store: Arc<HiqliteAuthStore>,
        metrics: PassiveRaftMetrics,
        client: &hiqlite::Client,
    ) -> Self {
        Self::replicated(authority, store, metrics, client)
    }

    /// Revoke the proof after exactly the next local statement and before
    /// its post-read validation.
    #[cfg(all(feature = "hiqlite-store", feature = "cluster-read-cost-validation"))]
    #[doc(hidden)]
    pub fn validation_revoke_after_next_local(&self) {
        if let Mode::Replicated(replicated) = &self.mode {
            replicated
                .revoke_after_next_local
                .store(true, Ordering::Relaxed);
        }
    }

    /// Record a fact this process learned at `at` (an acknowledged write to
    /// the table, or a quorum read of it): the view stays closed for that
    /// table until the replica is proven to contain it.
    pub fn note_fact(&self, table: AppliedTable, at: Instant) {
        self.floors.raise(table, at);
    }

    /// Whether this replica is proven to contain every fact this process
    /// learned at `at`. Always on SQLite, never on a remote-authority process.
    #[must_use]
    pub fn floor_met(&self, at: Instant) -> bool {
        match &self.mode {
            Mode::Standalone => true,
            Mode::RemoteAuthority => false,
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => {
                replicated.mark.generation() & 1 == 0 && replicated.floor_met(at)
            }
        }
    }

    /// Whether an entry made from an earlier view answer may still answer.
    /// Standalone and remote-authority views hand out no stamps.
    #[must_use]
    pub fn check(&self, stamp: &ViewStamp) -> StampCheck {
        match &self.mode {
            Mode::Standalone | Mode::RemoteAuthority => StampCheck::Stale,
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => replicated.check(stamp, self.floors.get(stamp.table)),
        }
    }

    /// Count one memo or cache reload a stale stamp caused.
    pub fn note_reload(&self, table: AppliedTable) {
        self.counters.reloads[table.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Run one local read under the rule, or say why Authority must answer.
    /// Counts nothing; the caller counts once it knows which source answered.
    #[cfg_attr(not(feature = "hiqlite-store"), allow(unused_variables))]
    async fn local<T, F, Fut>(
        &self,
        table: AppliedTable,
        local: F,
    ) -> Result<(T, Option<ViewStamp>), ViewFallback>
    where
        F: FnOnce(LocalReader) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        match &self.mode {
            Mode::Standalone => local(LocalReader::Standalone(Arc::clone(&self.authority)))
                .await
                .map(|value| (value, None))
                .map_err(|_| ViewFallback::LocalError),
            Mode::RemoteAuthority => Err(ViewFallback::NoProof),
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => replicated
                .read(table, self.floors.get(table), |store| {
                    local(LocalReader::Replica(store))
                })
                .await
                .map(|(value, stamp)| (value, Some(stamp))),
        }
    }

    /// One setting, from the view when it may answer.
    pub async fn setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self.settings(&[key]).await?.values.remove(key))
    }

    /// A boolean setting with the store's own parsing and default.
    pub async fn switch(&self, key: &str, default: bool) -> Result<bool, StoreError> {
        let value = self.setting(key).await?;
        Ok(super::stored_switch(value.as_deref(), default))
    }

    /// Several settings read together, with their digest.
    pub async fn settings(&self, keys: &[&str]) -> Result<AppliedSettings, StoreError> {
        let table = AppliedTable::Settings;
        let owned: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
        match self
            .local(table, |reader| async move {
                let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
                reader.settings(&keys).await
            })
            .await
        {
            Ok((values, stamp)) => {
                self.counters.view(table);
                Ok(AppliedSettings {
                    digest: SettingsDigest::of(keys, &values),
                    values,
                    source: ViewSource::View,
                    stamp,
                })
            }
            Err(reason) => {
                self.counters.authority(table, reason);
                let values = self.authority.get_settings(keys).await?;
                Ok(AppliedSettings {
                    digest: SettingsDigest::of(keys, &values),
                    values,
                    source: ViewSource::Authority,
                    stamp: None,
                })
            }
        }
    }

    /// Advisory facts, loaded now.
    #[must_use]
    pub fn observation(&self) -> AppliedViewObservation {
        self.metrics_handle().observation()
    }

    /// A store-free handle for `/metrics` and the Developer tab.
    #[must_use]
    pub fn metrics_handle(&self) -> AppliedViewMetrics {
        AppliedViewMetrics {
            mode: match &self.mode {
                Mode::Standalone => "standalone",
                Mode::RemoteAuthority => "remote_authority",
                #[cfg(feature = "hiqlite-store")]
                Mode::Replicated(_) => "replicated",
            },
            #[cfg(feature = "hiqlite-store")]
            replica: match &self.mode {
                Mode::Replicated(replicated) => {
                    Some((replicated.metrics.clone(), replicated.mark.clone()))
                }
                _ => None,
            },
            counters: Arc::clone(&self.counters),
        }
    }
}

/// What a local read closure is handed: the replica, or on SQLite the store.
enum LocalReader {
    Standalone(Arc<dyn Store>),
    #[cfg(feature = "hiqlite-store")]
    Replica(Arc<HiqliteAuthStore>),
}

impl LocalReader {
    async fn settings(&self, keys: &[&str]) -> Result<BTreeMap<String, String>, StoreError> {
        match self {
            Self::Standalone(store) => store.get_settings(keys).await,
            #[cfg(feature = "hiqlite-store")]
            Self::Replica(store) => store.local_get_settings(keys).await,
        }
    }
}

/// Store-free handle for `/metrics` and Settings → Developer.
#[derive(Clone)]
pub struct AppliedViewMetrics {
    mode: &'static str,
    #[cfg(feature = "hiqlite-store")]
    replica: Option<(PassiveRaftMetrics, hiqlite::AppliedMarkHandle)>,
    counters: Arc<ViewCounters>,
}

impl AppliedViewMetrics {
    /// Advisory facts, loaded now; no timer keeps them.
    #[must_use]
    pub fn observation(&self) -> AppliedViewObservation {
        let mut reads = Vec::with_capacity(2 * OUTCOMES);
        for table in AppliedTable::ALL {
            let row = &self.counters.reads[table.index()];
            reads.push((table.label(), "view", "ok", row[0].load(Ordering::Relaxed)));
            for reason in ViewFallback::ALL {
                reads.push((
                    table.label(),
                    "authority",
                    reason.label(),
                    row[usize::from(reason.code())].load(Ordering::Relaxed),
                ));
            }
        }
        let reloads = AppliedTable::ALL
            .map(|table| {
                (
                    table.label(),
                    self.counters.reloads[table.index()].load(Ordering::Relaxed),
                )
            })
            .to_vec();
        let mut observation = AppliedViewObservation {
            mode: self.mode,
            proof_present: self.mode == "standalone",
            generation: None,
            applied_index: None,
            failed_replacements: 0,
            revisions: Vec::new(),
            last_fallback: ViewFallback::from_code(
                self.counters.last_fallback.load(Ordering::Relaxed),
            ),
            reads,
            reloads,
        };
        #[cfg(feature = "hiqlite-store")]
        if let Some((metrics, mark)) = &self.replica {
            let mark = mark.observation();
            observation.proof_present = metrics.eligible_serving_proof().is_some();
            observation.generation = Some(mark.generation);
            observation.applied_index = Some(mark.applied_index);
            observation.failed_replacements = mark.failed_replacements;
            observation.revisions = AppliedTable::ALL
                .into_iter()
                .filter_map(|table| {
                    mark.tables
                        .iter()
                        .find(|(name, _)| name == table.table())
                        .map(|(_, revision)| (table.label(), *revision))
                })
                .collect();
        }
        observation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_digest_is_order_free_and_tells_absent_from_empty() {
        let mut values = BTreeMap::new();
        values.insert("b".to_owned(), "2".to_owned());
        values.insert("a".to_owned(), "1".to_owned());
        assert_eq!(
            SettingsDigest::of(&["a", "b"], &values),
            SettingsDigest::of(&["b", "a", "a"], &values)
        );
        let mut empty = values.clone();
        empty.insert("c".to_owned(), String::new());
        assert_ne!(
            SettingsDigest::of(&["a", "b", "c"], &values),
            SettingsDigest::of(&["a", "b", "c"], &empty),
            "an absent key and an empty value must not collide"
        );
        // Values the caller did not ask for do not enter the digest.
        assert_eq!(
            SettingsDigest::of(&["a"], &values),
            SettingsDigest::of(&["a"], &BTreeMap::from([("a".to_owned(), "1".to_owned())]))
        );
    }

    #[test]
    fn a_floor_only_rises() {
        let floors = TableFloors::new();
        assert_eq!(floors.get(AppliedTable::Settings), None);
        let later = Instant::now() + std::time::Duration::from_millis(5);
        floors.raise(AppliedTable::Settings, later);
        floors.raise(AppliedTable::Settings, floors.base);
        let kept = floors.get(AppliedTable::Settings).expect("raised");
        assert!(kept >= later - std::time::Duration::from_nanos(1));
        assert_eq!(floors.get(AppliedTable::MediaSessions), None);
    }

    #[test]
    fn every_reason_has_a_distinct_code_inside_the_counter_row() {
        let mut codes: Vec<u8> = ViewFallback::ALL
            .iter()
            .map(|reason| reason.code())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ViewFallback::ALL.len());
        assert!(codes
            .iter()
            .all(|code| *code != 0 && usize::from(*code) < OUTCOMES));
        for reason in ViewFallback::ALL {
            assert_eq!(ViewFallback::from_code(reason.code()), Some(reason));
        }
    }
}
