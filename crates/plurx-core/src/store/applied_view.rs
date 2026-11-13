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
//! **The settings view** (§4.3) is a memo of the whole `settings` table,
//! stamped like any view answer. Keyed reads answer from it while its stamp
//! is current and otherwise read their keys locally; only a whole-table
//! snapshot reloads it, and a snapshot that finds a reload in flight answers
//! from Authority rather than wait. The settings floor is raised by the
//! writers themselves: every replicated statement that may change `settings`
//! — directly or through a trigger — arms a [`SettingsWriteFloor`] guard
//! (`TimedClient` for the store, the membership manager's client for its own
//! statements), so the node that saved a setting reads it back from Authority
//! until its replica has applied the save.
//!
//! **The route view** (§4.4) answers one session-keyed route read
//! ([`AppliedView::route`]) from the replica once the principal session
//! layout is installed, and from Authority while the layout is legacy. It
//! keeps no memo: the daemon's route cache keeps the answer with its stamp,
//! and holds a fact it learned directly (a row it committed, a quorum read) as
//! an entry of its own whose instant it takes from [`AppliedView::observe`].
//! That fact is per key, so it does not close the view for the table; the
//! cache asks [`AppliedView::floor_met`] when to retire it, and
//! [`AppliedView::hold_floor`] closes the table only if it must drop such an
//! entry before then.
//!
//! **The planning view** (playback-start D3b,
//! `docs/streaming/PLAYBACK-START-D3-BUILD.md` §4.2) answers the create's
//! one planning snapshot — the file row, its probe, the planning settings and
//! `playback_input_generation`, in one statement — from the replica under
//! rules 1–3, **unstamped**: nothing caches it, so it needs no table slot,
//! only the image generation, the proof and two floors. The settings floor
//! covers the settings and the generation (only settings triggers write it);
//! the **files floor** is raised by every replicated statement that may write
//! `files` — directly, through a trigger ([`FILES_TRIGGER_TABLES`]) or by a
//! foreign-key cascade from a delete ([`FILES_CASCADE_TABLES`]) — the same
//! way the settings floor is, at `TimedClient`. The item title it carries is
//! attribution only and needs no floor.

use super::{Store, StoreError};
use crate::domain::MediaSessionRoute;
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

/// One applied view, by the table it reads (and whose floor it keeps).
///
/// `Files` is the planning view (D3b): it reads `files` with `settings`,
/// `items` and the planning generation in one unstamped statement, and owns
/// the files floor. It has no slot in the applied mark: nothing caches its
/// answer, so no table revision is needed to say when one goes stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppliedTable {
    Settings,
    MediaSessions,
    Files,
}

impl AppliedTable {
    pub const ALL: [Self; 3] = [Self::Settings, Self::MediaSessions, Self::Files];
    /// The views whose answers are stamped by a table revision of the
    /// applied mark: exactly the tables production registers
    /// ([`APPLIED_VIEW_TABLES`]).
    pub const STAMPED: [Self; 2] = [Self::Settings, Self::MediaSessions];

    /// The replicated table behind this view.
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::MediaSessions => "media_sessions",
            Self::Files => "files",
        }
    }

    /// The `view` label of `plurx_applied_view_*`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::MediaSessions => "route",
            Self::Files => "planning",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Settings => 0,
            Self::MediaSessions => 1,
            Self::Files => 2,
        }
    }

    const fn stamped(self) -> bool {
        !matches!(self, Self::Files)
    }
}

/// How many views (and floors) a process keeps.
const VIEWS: usize = AppliedTable::ALL.len();

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
    /// A snapshot found the memo stale while another reader was reloading
    /// it; it answers from Authority instead of waiting on that reload.
    ReloadInFlight,
    /// The planning view answered with a binding other than the one a peer
    /// planned at (D3b, §4.3): the owner asks Authority once.
    BindingMismatch,
    /// A legacy remote start carried no binding to check a view answer
    /// against (D3b, §4.3): the owner plans from Authority.
    Unbound,
}

impl ViewFallback {
    const ALL: [Self; 10] = [
        Self::NoProof,
        Self::ImageReplacing,
        Self::Floor,
        Self::RevokedDuringRead,
        Self::LegacyLayout,
        Self::LocalError,
        Self::Unregistered,
        Self::ReloadInFlight,
        Self::BindingMismatch,
        Self::Unbound,
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
            Self::ReloadInFlight => "reload_in_flight",
            Self::BindingMismatch => "binding_mismatch",
            Self::Unbound => "unbound",
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
            Self::ReloadInFlight => 8,
            Self::BindingMismatch => 9,
            Self::Unbound => 10,
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

/// [`AppliedView::route`]: the route, where it came from, and — for a view
/// answer on a replica — the stamp an entry made from it is checked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedRoute {
    pub route: Option<MediaSessionRoute>,
    pub stamp: Option<ViewStamp>,
    pub source: ViewSource,
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
    /// Whether the route view can answer locally: a replica whose session
    /// table has the installed principal layout and a registered slot.
    /// `None` without a replica (standalone, remote authority); `Some(false)`
    /// while the layout is legacy, when every route read is a quorum read.
    pub route_view_open: Option<bool>,
    /// `(view label, revision)` per registered view table.
    pub revisions: Vec<(&'static str, u64)>,
    pub last_fallback: Option<ViewFallback>,
    /// The last reason the planning view (D3b) answered from Authority.
    pub last_planning_fallback: Option<ViewFallback>,
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

    /// [`Self::total`] for one view (`settings` or `route`).
    #[must_use]
    pub fn view_total(&self, table: AppliedTable, source: ViewSource) -> u64 {
        let (view, label) = (table.label(), source_label(source));
        self.reads
            .iter()
            .filter(|(read_view, read_source, _, _)| *read_view == view && *read_source == label)
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
const OUTCOMES: usize = 11;

#[derive(Default)]
struct ViewCounters {
    reads: [[AtomicU64; OUTCOMES]; VIEWS],
    reloads: [AtomicU64; VIEWS],
    last_fallback: AtomicU8,
    /// The last reason the planning view answered from Authority, apart from
    /// the shared `last_fallback` (the Developer "Create plan" row).
    last_planning_fallback: AtomicU8,
}

impl ViewCounters {
    fn view(&self, table: AppliedTable) {
        self.reads[table.index()][0].fetch_add(1, Ordering::Relaxed);
    }

    fn authority(&self, table: AppliedTable, reason: ViewFallback) {
        self.reads[table.index()][usize::from(reason.code())].fetch_add(1, Ordering::Relaxed);
        self.last_fallback.store(reason.code(), Ordering::Relaxed);
        if table == AppliedTable::Files {
            self.last_planning_fallback
                .store(reason.code(), Ordering::Relaxed);
        }
    }
}

/// Per-table floors: the latest monotonic instant at which this process
/// learned a fact the replica must contain before the view may answer.
///
/// A replicated store owns one (its `TimedClient` raises the settings floor
/// on every settings write) and the applied view built over that store reads
/// the same one; [`AppliedView::note_fact`] raises it too.
pub(crate) struct TableFloors {
    base: Instant,
    /// Nanoseconds after `base`, plus one; zero means no floor.
    at: [AtomicU64; VIEWS],
}

impl TableFloors {
    pub(crate) fn new() -> Self {
        Self {
            base: Instant::now(),
            at: Default::default(),
        }
    }

    pub(crate) fn raise(&self, table: AppliedTable, at: Instant) {
        let nanos = u64::try_from(at.saturating_duration_since(self.base).as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1);
        self.at[table.index()].fetch_max(nanos, Ordering::AcqRel);
    }

    pub(crate) fn get(&self, table: AppliedTable) -> Option<Instant> {
        match self.at[table.index()].load(Ordering::Acquire) {
            0 => None,
            nanos => Some(self.base + std::time::Duration::from_nanos(nanos - 1)),
        }
    }
}

/// Tables whose triggers write `settings`, each with the SQL literal the
/// trigger's `WHEN` requires of the firing statement (`None`: any write
/// arms). A census over every trigger definition keeps this list true.
pub(crate) const SETTINGS_TRIGGER_TABLES: &[(&str, Option<&str>)] = &[
    // `subtitle_translation_settings_saved` (subtitle_translation.sql).
    ("subtitle_translation_commands", None),
    // `subtitle_managed_config_write` (subtitle_managed_schema.sql):
    // `WHEN NEW.operation='subtitle_managed_config'`.
    ("background_job_commands", Some("'subtitle_managed_config'")),
];

/// Whether a replicated statement may write `settings`: directly, or by
/// writing a table whose trigger does.
pub(crate) fn writes_settings(sql: &str) -> bool {
    use super::replicated::writes_table;
    writes_table(sql, AppliedTable::Settings.table())
        || SETTINGS_TRIGGER_TABLES.iter().any(|(table, when)| {
            writes_table(sql, table) && when.is_none_or(|literal| sql.contains(literal))
        })
}

/// Tables whose triggers write `files`, each with the SQL literal the
/// trigger's `WHEN` requires of the firing statement (`None`: any write
/// arms). A table may appear once per trigger discriminator. A census over
/// every trigger definition keeps this list true (D3b, §4.2).
pub(crate) const FILES_TRIGGER_TABLES: &[(&str, Option<&str>)] = &[
    // `dv_title_jobs_publish` (dv_title_jobs.sql):
    // `WHEN NEW.operation='publish_dv_title'`.
    ("background_job_commands", Some("'publish_dv_title'")),
    // `dv_title_jobs_bind` (dv_title_jobs.sql):
    // `WHEN NEW.operation='bind_dv_title'`.
    ("background_job_commands", Some("'bind_dv_title'")),
];

/// Tables whose deletes reach `files` through `ON DELETE` actions: `files`
/// references `items`, and `items` references `libraries` (and itself) with
/// `ON DELETE CASCADE`. A census over every `REFERENCES … ON DELETE` clause
/// keeps this list true.
pub(crate) const FILES_CASCADE_TABLES: &[&str] = &["items", "libraries"];

/// Whether a replicated statement may write `files`: directly, by writing a
/// table whose trigger does, or by deleting rows a cascade follows into it.
pub(crate) fn writes_files(sql: &str) -> bool {
    use super::replicated::{deletes_rows_of, writes_table};
    writes_table(sql, AppliedTable::Files.table())
        || FILES_TRIGGER_TABLES.iter().any(|(table, when)| {
            writes_table(sql, table) && when.is_none_or(|literal| sql.contains(literal))
        })
        || FILES_CASCADE_TABLES
            .iter()
            .any(|table| deletes_rows_of(sql, table))
}

/// Which write floors one replicated write raises: the classification of
/// its statements, taken before the write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WrittenTables {
    settings: bool,
    files: bool,
}

impl WrittenTables {
    /// One statement's floors.
    pub(crate) fn of(sql: &str) -> Self {
        Self {
            settings: writes_settings(sql),
            files: writes_files(sql),
        }
    }

    /// Every floor any of `statements` raises.
    pub(crate) fn any<'a>(statements: impl IntoIterator<Item = &'a str>) -> Self {
        statements
            .into_iter()
            .fold(Self::default(), |written, sql| {
                let one = Self::of(sql);
                Self {
                    settings: written.settings || one.settings,
                    files: written.files || one.files,
                }
            })
    }

    const fn is_empty(self) -> bool {
        !self.settings && !self.files
    }
}

/// This process's write floors (settings, and since D3b files), as the
/// writers that raise them hold them: the store's `TimedClient` and the
/// membership manager's client share one set with the applied view built
/// over that store (§4.3, read-your-write). The name is E1b's; the handle
/// carries every write floor.
#[derive(Clone)]
pub struct SettingsWriteFloor(Arc<TableFloors>);

impl SettingsWriteFloor {
    /// A floor no applied view reads, for a client outside a daemon.
    #[must_use]
    pub fn detached() -> Self {
        Self(Arc::new(TableFloors::new()))
    }

    pub(crate) fn floors(&self) -> Arc<TableFloors> {
        Arc::clone(&self.0)
    }

    /// Arm the floor for one write whose statements are `statements`. The
    /// guard raises it when dropped — after the write returned, acknowledged
    /// or failed, or when its caller went away — never before: a watermark
    /// confirmation that started before the commit may carry a read index
    /// below it. A write that commits only after the guard dropped (one that
    /// timed out here and committed later) is not covered; its caller was
    /// never told it succeeded, and read-your-write is owed for acknowledged
    /// writes.
    pub(crate) fn arm<'a>(
        &self,
        statements: impl IntoIterator<Item = &'a str>,
    ) -> WriteFloorGuard {
        self.guard(WrittenTables::any(statements))
    }

    /// The guard for a write already classified ([`WrittenTables`]).
    pub(crate) fn guard(&self, written: WrittenTables) -> WriteFloorGuard {
        WriteFloorGuard {
            floors: (!written.is_empty()).then(|| Arc::clone(&self.0)),
            written,
        }
    }
}

/// Raises the floors of the tables a write may have changed when dropped;
/// see [`SettingsWriteFloor::arm`].
#[must_use = "the floor is raised when the guard drops: hold it across the write"]
pub(crate) struct WriteFloorGuard {
    floors: Option<Arc<TableFloors>>,
    written: WrittenTables,
}

impl Drop for WriteFloorGuard {
    fn drop(&mut self) {
        if let Some(floors) = &self.floors {
            let now = Instant::now();
            if self.written.settings {
                floors.raise(AppliedTable::Settings, now);
            }
            if self.written.files {
                floors.raise(AppliedTable::Files, now);
            }
        }
    }
}

#[cfg(feature = "hiqlite-store")]
struct Replicated {
    store: Arc<HiqliteAuthStore>,
    metrics: PassiveRaftMetrics,
    mark: hiqlite::AppliedMarkHandle,
    slots: [Option<hiqlite::AppliedSlot>; VIEWS],
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

    /// Step 1 for an unstamped read (D3b): no slot and no revision — nothing
    /// caches the answer — so only the image generation, the proof and every
    /// floor the read depends on.
    fn admit_unstamped(&self, floors: &[Option<Instant>]) -> Result<(ServingProof, u64), ViewFallback> {
        let generation = self.mark.generation();
        if generation & 1 != 0 {
            return Err(ViewFallback::ImageReplacing);
        }
        let proof = self
            .metrics
            .eligible_serving_proof()
            .ok_or(ViewFallback::NoProof)?;
        if floors.iter().flatten().any(|at| !self.floor_met(*at)) {
            return Err(ViewFallback::Floor);
        }
        Ok((proof, generation))
    }

    /// Steps 1-3 around one unstamped local statement.
    async fn read_unstamped<T, F, Fut>(
        &self,
        floors: &[Option<Instant>],
        local: F,
    ) -> Result<T, ViewFallback>
    where
        F: FnOnce(Arc<HiqliteAuthStore>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        let (proof, generation) = self.admit_unstamped(floors)?;
        let result = local(Arc::clone(&self.store)).await;
        #[cfg(feature = "cluster-read-cost-validation")]
        if self.revoke_after_next_local.swap(false, Ordering::Relaxed) {
            self.metrics.validation_revoke_bounded_proof();
        }
        let value = result.map_err(|_| ViewFallback::LocalError)?;
        if self.mark.generation() != generation {
            return Err(ViewFallback::ImageReplacing);
        }
        if !self.metrics.serving_proof_remains_valid(&proof) {
            return Err(ViewFallback::RevokedDuringRead);
        }
        Ok(value)
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

    /// Whether the route view can answer from this replica at all: the
    /// route slot is registered and the principal session layout installed.
    /// While it cannot, every route read is Authority's.
    fn answers_routes(&self) -> bool {
        self.slots[AppliedTable::MediaSessions.index()].is_some()
            && self.store.media_session_layout_installed()
    }

    /// Whether the image and the stamp's table revision are what they were
    /// when the stamp was taken; the proof and floor are not consulted.
    fn same_image(&self, stamp: &ViewStamp) -> bool {
        self.slot(stamp.table).is_ok_and(|slot| {
            self.mark.generation() == stamp.generation && self.mark.revision(slot) == stamp.revision
        })
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

/// The monotonic instant a fact became known to this process, taken by
/// [`AppliedView::note_fact`] or [`AppliedView::observe`] after the
/// acknowledgement. Those are its only constructors, so a caller cannot hand
/// the floor an earlier instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactInstant(Instant);

/// The settings view (§4.3): the whole table as this replica applied it,
/// with the stamp of the read that loaded it. One snapshot reloads it at a
/// time; others answer from Authority meanwhile.
#[derive(Default)]
#[cfg_attr(not(feature = "hiqlite-store"), allow(dead_code))]
struct SettingsMemo {
    current: std::sync::Mutex<Option<MemoEntry>>,
    reload: tokio::sync::Mutex<()>,
    /// Holds the next reload, lock held, until released.
    #[cfg(feature = "cluster-read-cost-validation")]
    pause: std::sync::Mutex<Option<ReloadPause>>,
}

#[cfg(feature = "cluster-read-cost-validation")]
type ReloadPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Clone)]
#[cfg_attr(not(feature = "hiqlite-store"), allow(dead_code))]
struct MemoEntry {
    values: Arc<BTreeMap<String, String>>,
    stamp: ViewStamp,
}

impl SettingsMemo {
    fn current(&self) -> Option<MemoEntry> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg_attr(not(feature = "hiqlite-store"), allow(dead_code))]
    fn replace(&self, entry: MemoEntry) {
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(entry);
    }
}

/// [`AppliedView::settings_snapshot`]: every setting, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedSnapshot {
    pub values: Arc<BTreeMap<String, String>>,
    pub source: ViewSource,
}

/// Why a local read did not answer.
enum LocalFailure {
    /// The view may not answer; Authority does, once.
    Authority(ViewFallback),
    /// The only copy failed (SQLite): there is nothing else to ask.
    Store(StoreError),
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
    memo: Arc<SettingsMemo>,
}

impl AppliedView {
    /// SQLite (and SQLite recovery): the store is the only copy, so every
    /// read forwards to it and every proof and floor is met.
    #[must_use]
    pub fn authority(store: Arc<dyn Store>) -> Self {
        Self::with_mode(store, Mode::Standalone, Arc::new(TableFloors::new()))
    }

    /// A replicated process without a local replica: every read is a quorum
    /// read and no floor is ever met.
    #[must_use]
    pub fn remote_authority(store: Arc<dyn Store>) -> Self {
        Self::with_mode(store, Mode::RemoteAuthority, Arc::new(TableFloors::new()))
    }

    fn with_mode(authority: Arc<dyn Store>, mode: Mode, floors: Arc<TableFloors>) -> Self {
        Self {
            authority,
            mode,
            floors,
            counters: Arc::new(ViewCounters::default()),
            memo: Arc::new(SettingsMemo::default()),
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
        let slots = AppliedTable::ALL.map(|table| {
            if table.stamped() {
                mark.slot(table.table())
            } else {
                None
            }
        });
        if AppliedTable::STAMPED
            .iter()
            .any(|table| slots[table.index()].is_none())
        {
            tracing::error!(
                registered = ?mark.observation().tables,
                "the applied mark does not register every applied-view table; those views \
                 answer from Authority"
            );
        }
        // The store's own floors: its writes raise the settings floor.
        let floors = store.settings_write_floor().floors();
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
            floors,
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

    /// Hold the next memo reload, with the reload lock taken, until the
    /// returned sender fires; the receiver fires once it is held.
    #[cfg(all(feature = "hiqlite-store", feature = "cluster-read-cost-validation"))]
    #[doc(hidden)]
    pub fn validation_pause_next_reload(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, held) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *self
            .memo
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((entered, released));
        (held, release)
    }

    /// Record that this process now holds a fact about `table` that the
    /// replica may not: call it **after** the write was acknowledged, or
    /// after the quorum read returned, and before the result is used.
    ///
    /// The instant is taken here, not by the caller, because the floor is
    /// only sound for an instant no earlier than the acknowledgement: a
    /// watermark confirmation that started between an earlier instant and
    /// the commit may carry a read index below the write. The view stays
    /// closed for `table` until a confirmation started after the returned
    /// instant has been applied here; the returned [`FactInstant`] is what an
    /// E1c `Observed` cache entry keeps to ask [`Self::floor_met`].
    pub fn note_fact(&self, table: AppliedTable) -> FactInstant {
        let at = Instant::now();
        self.floors.raise(table, at);
        FactInstant(at)
    }

    /// The instant of a fact about **one row** that the caller keeps in an
    /// entry of its own — the route cache's `Observed` entry (§4.4) — rather
    /// than closing the view for the whole table: call it, like
    /// [`Self::note_fact`], after the write was acknowledged or the quorum
    /// read returned. The caller must not read that row through the view
    /// until [`Self::floor_met`] says the replica contains the fact; if it
    /// drops the entry before then, [`Self::hold_floor`] keeps the promise
    /// for the table.
    #[must_use]
    pub fn observe(&self) -> FactInstant {
        FactInstant(Instant::now())
    }

    /// Close the view for `table` until `fact` is applied here: for a caller
    /// that drops the per-row entry holding `fact` ([`Self::observe`]) while
    /// its floor is still unmet, so no later read of that row can come from a
    /// replica older than the fact.
    pub fn hold_floor(&self, table: AppliedTable, fact: FactInstant) {
        self.floors.raise(table, fact.0);
    }

    /// Whether the image and the table revision a stamp was taken at are
    /// still current — [`Self::check`] without the proof and floor, which
    /// are the same for every entry of a table. `false` on SQLite and on a
    /// remote-authority process, which hand out no stamps.
    #[must_use]
    pub fn same_image(&self, stamp: &ViewStamp) -> bool {
        match &self.mode {
            Mode::Standalone | Mode::RemoteAuthority => false,
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => replicated.same_image(stamp),
        }
    }

    /// Whether [`Self::route`] can answer from this node's replica: a
    /// replicated process whose route slot is registered and whose session
    /// table has the installed principal layout. A per-row fact may be
    /// retired into the view only while this holds; otherwise retiring it
    /// would only trade the fact for another Authority read. `false` on
    /// SQLite (the store answers, unstamped) and on a remote-authority process.
    #[must_use]
    pub fn answers_routes(&self) -> bool {
        match &self.mode {
            Mode::Standalone | Mode::RemoteAuthority => false,
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => replicated.answers_routes(),
        }
    }

    /// One session-keyed route from Authority — a quorum read on a
    /// replicated node, the store itself on SQLite — for a caller replacing
    /// a per-row fact whose lease ended before the replica was proven to
    /// contain it. Not counted as a view read.
    pub async fn authority_route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        self.authority.media_session_route(session_id).await
    }

    /// Whether the view may answer for a fact noted at `fact`: the image is
    /// stable, a serving proof is held (so the watermark is from the current
    /// leader, term and protocol) and a confirmation started after `fact` is
    /// applied here. Always on SQLite, never on a remote-authority process.
    #[must_use]
    pub fn floor_met(&self, fact: FactInstant) -> bool {
        match &self.mode {
            Mode::Standalone => true,
            Mode::RemoteAuthority => false,
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => {
                replicated.mark.generation() & 1 == 0
                    && replicated.metrics.eligible_serving_proof().is_some()
                    && replicated.floor_met(fact.0)
            }
        }
    }

    /// Whether an entry made from an earlier view answer may still answer.
    /// Standalone and remote-authority views hand out no stamps: on SQLite
    /// every view read goes to the store, which is the only copy and a local
    /// read (§4.2 "Standalone"), so nothing is cached that a write would
    /// have to invalidate.
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
    ) -> Result<(T, Option<ViewStamp>), LocalFailure>
    where
        F: FnOnce(LocalReader) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        match &self.mode {
            Mode::Standalone => local(LocalReader::Standalone(Arc::clone(&self.authority)))
                .await
                .map(|value| (value, None))
                .map_err(LocalFailure::Store),
            Mode::RemoteAuthority => Err(LocalFailure::Authority(ViewFallback::NoProof)),
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => replicated
                .read(table, self.floors.get(table), |store| {
                    local(LocalReader::Replica(store))
                })
                .await
                .map(|(value, stamp)| (value, Some(stamp)))
                .map_err(LocalFailure::Authority),
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

    /// Several settings read together, as [`super::SettingsStore::get_settings`]
    /// returns them.
    pub async fn setting_values(
        &self,
        keys: &[&str],
    ) -> Result<BTreeMap<String, String>, StoreError> {
        Ok(self.settings(keys).await?.values)
    }

    /// Two settings read together.
    pub async fn setting_pair(
        &self,
        first: &str,
        second: &str,
    ) -> Result<(Option<String>, Option<String>), StoreError> {
        let mut values = self.settings(&[first, second]).await?.values;
        Ok((values.remove(first), values.remove(second)))
    }

    /// Several settings read together, with their digest. On a replica the
    /// answer comes from the memo; on SQLite from the store, keyed.
    pub async fn settings(&self, keys: &[&str]) -> Result<AppliedSettings, StoreError> {
        let table = AppliedTable::Settings;
        let local = match &self.mode {
            Mode::Standalone => {
                let owned: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
                self.local(table, |reader| async move {
                    let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
                    reader.settings(&keys).await
                })
                .await
            }
            _ => {
                // The same bound Authority enforces, so the two never differ
                // in what they accept.
                super::selected_settings_json(keys)?;
                match self.memo_hit() {
                    Ok(Some((all, stamp))) => {
                        let values = keys
                            .iter()
                            .filter_map(|key| {
                                all.get(*key)
                                    .map(|value| ((*key).to_owned(), value.clone()))
                            })
                            .collect();
                        Ok((values, stamp))
                    }
                    Err(failure) => Err(failure),
                    // A stale memo is not reloaded for a keyed read: settings
                    // also hold job cursors and stamps, and a whole-table load
                    // per change would make the request path pay for them.
                    // The keys are read locally, under the same rule.
                    Ok(None) => {
                        let owned: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
                        self.local(table, |reader| async move {
                            let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
                            reader.settings(&keys).await
                        })
                        .await
                    }
                }
            }
        };
        match local {
            Ok((values, stamp)) => {
                self.counters.view(table);
                Ok(AppliedSettings {
                    digest: SettingsDigest::of(keys, &values),
                    values,
                    source: ViewSource::View,
                    stamp,
                })
            }
            // SQLite: the store is the only copy; asking it again is a retry.
            Err(LocalFailure::Store(error)) => Err(error),
            Err(LocalFailure::Authority(reason)) => {
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

    /// The whole settings table: the administrative pages' one snapshot.
    pub async fn settings_snapshot(&self) -> Result<AppliedSnapshot, StoreError> {
        let table = AppliedTable::Settings;
        match self.settings_memo().await {
            Ok((values, _)) => {
                self.counters.view(table);
                Ok(AppliedSnapshot {
                    values,
                    source: ViewSource::View,
                })
            }
            Err(LocalFailure::Store(error)) => Err(error),
            Err(LocalFailure::Authority(reason)) => {
                self.counters.authority(table, reason);
                Ok(AppliedSnapshot {
                    values: Arc::new(self.authority.settings_snapshot().await?),
                    source: ViewSource::Authority,
                })
            }
        }
    }

    /// The whole table from the view, or why Authority must answer: a memo
    /// hit while its stamp is current, else one local reload under §4.2's
    /// rule unless another is in flight. Counts reloads, not reads.
    async fn settings_memo(
        &self,
    ) -> Result<(Arc<BTreeMap<String, String>>, Option<ViewStamp>), LocalFailure> {
        match &self.mode {
            Mode::Standalone => self
                .authority
                .settings_snapshot()
                .await
                .map(|values| (Arc::new(values), None))
                .map_err(LocalFailure::Store),
            Mode::RemoteAuthority => Err(LocalFailure::Authority(ViewFallback::NoProof)),
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => {
                if let Some(hit) = self.memo_hit()? {
                    return Ok(hit);
                }
                // No convoy behind a slow or failing reload (§4.2: no wait):
                // while one is in flight, this read is a quorum read.
                let Ok(_reload) = self.memo.reload.try_lock() else {
                    return Err(LocalFailure::Authority(ViewFallback::ReloadInFlight));
                };
                // Another reader may have just finished a reload.
                if let Some(hit) = self.memo_hit()? {
                    return Ok(hit);
                }
                #[cfg(feature = "cluster-read-cost-validation")]
                {
                    let pause = self
                        .memo
                        .pause
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some((entered, release)) = pause {
                        let _ = entered.send(());
                        let _ = release.await;
                    }
                }
                let table = AppliedTable::Settings;
                let (values, stamp) = replicated
                    .read(table, self.floors.get(table), |store| async move {
                        store.local_settings_snapshot().await
                    })
                    .await
                    .map_err(LocalFailure::Authority)?;
                let values = Arc::new(values);
                self.note_reload(table);
                self.memo.replace(MemoEntry {
                    values: Arc::clone(&values),
                    stamp,
                });
                Ok((values, Some(stamp)))
            }
        }
    }

    /// The memo, while its stamp is current; `None` when it is stale or
    /// absent. Standalone and remote-authority views never fill it.
    #[allow(clippy::type_complexity)]
    fn memo_hit(
        &self,
    ) -> Result<Option<(Arc<BTreeMap<String, String>>, Option<ViewStamp>)>, LocalFailure> {
        let Some(entry) = self.memo.current() else {
            return Ok(None);
        };
        match self.check(&entry.stamp) {
            StampCheck::Current => Ok(Some((entry.values, Some(entry.stamp)))),
            StampCheck::Stale => Ok(None),
            StampCheck::Closed(reason) => Err(LocalFailure::Authority(reason)),
        }
    }

    /// One session-keyed route (§4.4): [`super::MediaSessionStore::media_session_route`]
    /// from the replica when the view may answer, else from Authority. The
    /// local twin runs only once this process has seen the installed
    /// principal session layout; while the layout is legacy (or not yet
    /// read) the answer is Authority's, whose read re-checks it. On SQLite
    /// the store answers, unstamped.
    pub async fn route(&self, session_id: &str) -> Result<AppliedRoute, StoreError> {
        let table = AppliedTable::MediaSessions;
        let local = match &self.mode {
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) if !replicated.store.media_session_layout_installed() => {
                Err(LocalFailure::Authority(ViewFallback::LegacyLayout))
            }
            _ => {
                let owned = session_id.to_owned();
                self.local(table, |reader| async move { reader.route(&owned).await })
                    .await
            }
        };
        match local {
            Ok((route, stamp)) => {
                self.counters.view(table);
                Ok(AppliedRoute {
                    route,
                    stamp,
                    source: ViewSource::View,
                })
            }
            // SQLite: the store is the only copy; asking it again is a retry.
            Err(LocalFailure::Store(error)) => Err(error),
            Err(LocalFailure::Authority(reason)) => {
                self.counters.authority(table, reason);
                Ok(AppliedRoute {
                    route: self.authority.media_session_route(session_id).await?,
                    stamp: None,
                    source: ViewSource::Authority,
                })
            }
        }
    }

    /// The create's planning snapshot (D3b, §4.2): the file row, its probe,
    /// `keys` and `playback_input_generation` from one statement, read on the
    /// replica under rules 1–3 behind the settings and files floors, else
    /// from Authority. Unstamped: the caller keeps it for one request and
    /// binds it with its generation. On SQLite the store answers.
    pub async fn planning(
        &self,
        file_id: i64,
        keys: &[&str],
    ) -> Result<Option<(super::PlaybackPlanningSnapshot, ViewSource)>, StoreError> {
        let table = AppliedTable::Files;
        let local = match &self.mode {
            Mode::Standalone => {
                let snapshot = self.authority.playback_planning_snapshot(file_id, keys).await?;
                self.counters.view(table);
                return Ok(snapshot.map(|snapshot| (snapshot, ViewSource::View)));
            }
            Mode::RemoteAuthority => Err(ViewFallback::NoProof),
            #[cfg(feature = "hiqlite-store")]
            Mode::Replicated(replicated) => {
                // The same bound Authority enforces, so the two never differ
                // in what they accept.
                super::selected_planning_keys_json(keys)?;
                let owned: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
                replicated
                    .read_unstamped(
                        &[
                            self.floors.get(AppliedTable::Settings),
                            self.floors.get(AppliedTable::Files),
                        ],
                        |store| async move {
                            let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
                            store.local_playback_planning_snapshot(file_id, &keys).await
                        },
                    )
                    .await
            }
        };
        match local {
            Ok(snapshot) => {
                self.counters.view(table);
                Ok(snapshot.map(|snapshot| (snapshot, ViewSource::View)))
            }
            Err(reason) => self.planning_from_authority(file_id, keys, reason).await,
        }
    }

    /// The planning snapshot from Authority, counted under `reason`: for an
    /// owner whose view answer did not match the binding it was sent
    /// ([`ViewFallback::BindingMismatch`]) or that was sent none
    /// ([`ViewFallback::Unbound`]), and for [`Self::planning`]'s own
    /// fallbacks.
    pub async fn planning_from_authority(
        &self,
        file_id: i64,
        keys: &[&str],
        reason: ViewFallback,
    ) -> Result<Option<(super::PlaybackPlanningSnapshot, ViewSource)>, StoreError> {
        self.counters.authority(AppliedTable::Files, reason);
        Ok(self
            .authority
            .playback_planning_snapshot(file_id, keys)
            .await?
            .map(|snapshot| (snapshot, ViewSource::Authority)))
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
                Mode::Replicated(replicated) => Some(Arc::clone(replicated)),
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

    async fn route(&self, session_id: &str) -> Result<Option<MediaSessionRoute>, StoreError> {
        match self {
            Self::Standalone(store) => store.media_session_route(session_id).await,
            #[cfg(feature = "hiqlite-store")]
            Self::Replica(store) => store.local_media_session_route(session_id).await,
        }
    }
}

/// Store-free handle for `/metrics` and Settings → Developer.
#[derive(Clone)]
pub struct AppliedViewMetrics {
    mode: &'static str,
    #[cfg(feature = "hiqlite-store")]
    replica: Option<Arc<Replicated>>,
    counters: Arc<ViewCounters>,
}

impl AppliedViewMetrics {
    /// Advisory facts, loaded now; no timer keeps them.
    #[must_use]
    pub fn observation(&self) -> AppliedViewObservation {
        let mut reads = Vec::with_capacity(VIEWS * OUTCOMES);
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
            route_view_open: None,
            revisions: Vec::new(),
            last_fallback: ViewFallback::from_code(
                self.counters.last_fallback.load(Ordering::Relaxed),
            ),
            last_planning_fallback: ViewFallback::from_code(
                self.counters.last_planning_fallback.load(Ordering::Relaxed),
            ),
            reads,
            reloads,
        };
        #[cfg(feature = "hiqlite-store")]
        if let Some(replicated) = &self.replica {
            let mark = replicated.mark.observation();
            observation.proof_present = replicated.metrics.eligible_serving_proof().is_some();
            observation.generation = Some(mark.generation);
            observation.applied_index = Some(mark.applied_index);
            observation.failed_replacements = mark.failed_replacements;
            observation.route_view_open = Some(replicated.answers_routes());
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

    /// The floor's instant is taken by `note_fact` itself, after the call
    /// begins: a caller cannot backdate it to before its acknowledgement.
    #[test]
    fn a_fact_is_stamped_when_it_is_noted() {
        let store: Arc<dyn Store> =
            Arc::new(crate::store::SqliteStore::open_in_memory().expect("store"));
        let view = AppliedView::authority(store);
        let before = Instant::now();
        let fact = view.note_fact(AppliedTable::Settings);
        let after = Instant::now();
        assert!(
            before <= fact.0 && fact.0 <= after,
            "{before:?} {fact:?} {after:?}"
        );
        assert_eq!(view.floors.get(AppliedTable::Settings), Some(fact.0));
        assert_eq!(view.floors.get(AppliedTable::MediaSessions), None);
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

    /// The settings floor's classifier: every way plurx writes `settings`
    /// arms it, reads and look-alike names do not.
    #[test]
    fn a_settings_write_is_recognised_in_every_form_plurx_uses() {
        use crate::store::replicated::writes_table;
        for sql in [
            "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3)",
            "insert or replace into settings(key,value) values($1,'1')",
            "REPLACE INTO settings VALUES ($1, $2, $3)",
            "UPDATE settings SET value = $1 WHERE key = $2",
            "UPDATE OR IGNORE settings SET value = $1",
            "DELETE FROM settings\n             WHERE key <> $1",
            "delete from main.settings where key = $1",
            "UPDATE main . settings SET value = $1",
            "INSERT INTO \"settings\" (key, value) VALUES ($1, $2)",
            "WITH v(k) AS (SELECT $1) INSERT INTO settings SELECT k, 'x', 0 FROM v",
            "CREATE TRIGGER t AFTER INSERT ON files BEGIN DELETE FROM settings WHERE key = NEW.k; END",
        ] {
            assert!(writes_table(sql, "settings"), "{sql}");
        }
        for sql in [
            "SELECT key, value FROM settings ORDER BY key",
            "INSERT INTO libraries (name) SELECT value FROM settings WHERE key = $1",
            "UPDATE media_sessions SET lease = $1 WHERE (SELECT value FROM settings) = '1'",
            "INSERT INTO transcode_settings (k) VALUES ($1)",
            "UPDATE settings_history SET v = 1",
            "INSERT INTO notes (body) VALUES ('update settings now')",
            "DELETE FROM files -- then DELETE FROM settings\n WHERE id = $1",
            "/* INSERT INTO settings */ DELETE FROM files WHERE id = $1",
        ] {
            assert!(!writes_table(sql, "settings"), "{sql}");
        }
    }

    /// The files floor's classifier (D3b): direct writes, the two dv-title
    /// command triggers and the deletes that cascade into `files` arm it;
    /// reads, other commands, item updates and look-alike names do not.
    #[test]
    fn a_files_write_is_recognised_in_every_form_plurx_uses() {
        for sql in [
            "UPDATE files SET probe_json = $1 WHERE id = $2",
            "INSERT INTO files (item_id, path) VALUES ($1, $2) ON CONFLICT(path) DO UPDATE SET size = 1",
            "DELETE FROM files WHERE id IN (SELECT value FROM json_each($1))",
            "WITH admitted AS (SELECT 1) UPDATE files SET scanned_at = 1 WHERE id = 1",
            "INSERT INTO background_job_commands(id,operation,request_json,result_json) \
             SELECT $1,'publish_dv_title',$2,$3",
            "INSERT INTO background_job_commands(id,operation) SELECT $1,'bind_dv_title'",
            "DELETE FROM items WHERE library_id = $1 AND id NOT IN (SELECT item_id FROM files)",
            "delete from libraries where id = $1",
            "INSERT OR REPLACE INTO items (id, title) VALUES ($1, $2)",
            "REPLACE INTO items VALUES ($1)",
        ] {
            assert!(writes_files(sql), "{sql}");
            assert!(WrittenTables::of(sql).files, "{sql}");
        }
        for sql in [
            "SELECT probe_json FROM files WHERE id = $1",
            "INSERT INTO background_job_commands(id,operation) SELECT $1,'subtitle_managed_config'",
            "UPDATE items SET title = $1 WHERE id = $2",
            "INSERT INTO items (library_id, title) VALUES ($1, $2)",
            "INSERT INTO items (library_id, title) VALUES ($1, $2) ON CONFLICT DO UPDATE SET title = 1",
            "DELETE FROM items_fts WHERE rowid = $1",
            "INSERT INTO files_history (id) VALUES ($1)",
            "DELETE FROM watch_state WHERE item_id IN (SELECT id FROM items)",
        ] {
            assert!(!writes_files(sql), "{sql}");
        }
        let written = WrittenTables::any([
            "UPDATE settings SET value = $1 WHERE key = $2",
            "SELECT 1 FROM files",
        ]);
        assert_eq!(
            written,
            WrittenTables {
                settings: true,
                files: false
            }
        );
        assert_eq!(
            WrittenTables::any(["UPDATE settings SET value = 1", "DELETE FROM files"]),
            WrittenTables {
                settings: true,
                files: true
            }
        );
    }

    /// The production statements behind the trigger and probe paths the
    /// contract names (§8 "files floor") classify as `files` writes: the
    /// dv-title bind and both publish commands fire `dv_title_jobs_*`
    /// triggers, and the admitted probe result updates `files` directly.
    /// `TimedClient` classifies every statement it runs with this function.
    #[test]
    fn the_dv_title_commands_and_the_probe_result_raise_the_files_floor() {
        let commands: Vec<&str> = sql_texts("store/dv_title_jobs.rs", include_str!("dv_title_jobs.rs"))
            .into_iter()
            .filter(|sql| {
                sql.contains("INSERT INTO background_job_commands")
                    && (sql.contains("'publish_dv_title'") || sql.contains("'bind_dv_title'"))
            })
            .collect();
        assert_eq!(commands.len(), 3, "bind, publish and analysis publish");
        for sql in commands {
            assert!(writes_files(sql), "{sql}");
        }
        let probe: Vec<&str> = sql_texts(
            "store/background_jobs_probe.rs",
            include_str!("background_jobs_probe.rs"),
        )
        .into_iter()
        .filter(|sql| sql.contains("UPDATE files SET"))
        .collect();
        assert!(!probe.is_empty(), "the admitted probe result");
        for sql in probe {
            assert!(writes_files(sql), "{sql}");
        }
    }

    /// A guard raises exactly the floors of the tables its write may change,
    /// and only when it drops.
    #[test]
    fn a_write_guard_raises_only_its_own_floors() {
        let floor = SettingsWriteFloor::detached();
        let floors = floor.floors();
        let guard = floor.guard(WrittenTables::of("DELETE FROM items WHERE id = $1"));
        assert_eq!(floors.get(AppliedTable::Files), None, "raised before the write ended");
        drop(guard);
        assert!(floors.get(AppliedTable::Files).is_some());
        assert_eq!(floors.get(AppliedTable::Settings), None);
        drop(floor.guard(WrittenTables::of("SELECT 1 FROM files")));
        assert_eq!(floors.get(AppliedTable::Settings), None);
        drop(floor.arm(["UPDATE settings SET value = 1 WHERE key = 'x'"]));
        assert!(floors.get(AppliedTable::Settings).is_some());
    }

    /// Every replicated write method of the store's one client arms its
    /// write floors (settings and files), holds the guard across the write
    /// and drops it after: whatever its visibility, and however the write is
    /// spelled.
    #[test]
    fn every_timed_client_write_arms_the_settings_floor() {
        let source = include_str!("hiqlite.rs");
        let start = source
            .find("impl TimedClient {")
            .expect("the TimedClient impl");
        let end = start
            + source[start..]
                .find("\n}\n")
                .expect("the end of the TimedClient impl");
        let block = &source[start..end];
        let mut methods = Vec::new();
        let mut at = 0;
        while let Some(offset) = block[at..].find("fn ") {
            let begin = at + offset;
            let line_start = block[..begin].rfind('\n').map_or(0, |index| index + 1);
            // A method of the impl: `fn` at the impl's indentation, after an
            // optional visibility and `async`.
            let prefix = &block[line_start..begin];
            if prefix.starts_with("    ")
                && !prefix.starts_with("     ")
                && prefix
                    .split_whitespace()
                    .all(|word| word.starts_with("pub") || word == "async")
            {
                methods.push(line_start);
            }
            at = begin + 3;
        }
        methods.push(block.len());
        let mut writers = Vec::new();
        for window in methods.windows(2) {
            let method = &block[window[0]..window[1]];
            let name = method
                .split("fn ")
                .nth(1)
                .and_then(|rest| rest.split(['<', '(']).next())
                .unwrap_or_default();
            let compact: String = method.chars().filter(|c| !c.is_whitespace()).collect();
            let write = [
                "self.inner().execute",
                "self.inner().txn(",
                "self.inner().batch(",
            ]
            .iter()
            .filter_map(|call| compact.rfind(call))
            .max();
            let Some(write) = write else { continue };
            writers.push(name.to_owned());
            let armed = compact
                .find("letfloors=self.write_floors(WrittenTables::")
                .unwrap_or_else(|| panic!("TimedClient::{name} writes without arming the floor"));
            let dropped = compact
                .rfind("drop(floors);")
                .unwrap_or_else(|| panic!("TimedClient::{name} never drops its guard"));
            assert!(
                armed < write && write < dropped,
                "TimedClient::{name} must arm before the write and drop the guard after it"
            );
        }
        writers.sort();
        assert_eq!(
            writers,
            [
                "batch",
                "execute",
                "execute_acked",
                "execute_idempotent",
                "execute_returning_map",
                "execute_returning_map_acked",
                "execute_returning_map_one",
                "txn",
                "txn_source_layout_attempt",
            ],
            "the census of TimedClient write methods moved; review the new one"
        );
    }

    /// The crate's production sources under `dir` (relative to `src/`):
    /// `(path, text)`, test files left out and inline test modules cut.
    fn production_sources(root: &std::path::Path, dir: &str) -> Vec<(String, String)> {
        fn walk(path: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(path).expect("read a source directory") {
                let path = entry.expect("a source entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&root.join(dir), &mut files);
        files.sort();
        files
            .into_iter()
            .filter_map(|path| {
                let rel = path
                    .strip_prefix(root)
                    .expect("under the source root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let sql = rel.ends_with(".sql");
                let stem = rel
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(".rs")
                    .trim_end_matches(".sql");
                let test = rel.split('/').any(|part| part == "tests")
                    || stem == "tests"
                    || stem.ends_with("_tests")
                    || stem.ends_with("_test")
                    || stem.starts_with("test_");
                if !(sql || rel.ends_with(".rs")) || test {
                    return None;
                }
                let text = std::fs::read_to_string(&path).expect("read a source file");
                let text = match text.find("\n#[cfg(test)]\nmod tests") {
                    Some(cut) if !sql => text[..cut].to_owned(),
                    _ => text,
                };
                Some((rel, text))
            })
            .collect()
    }

    /// The SQL a source holds: a `.sql` file whole, a Rust file's string
    /// literals (plain, raw and byte).
    fn sql_texts<'a>(path: &str, text: &'a str) -> Vec<&'a str> {
        if path.ends_with(".sql") {
            return vec![text];
        }
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            let rest = &text[index..];
            if rest.starts_with("//") {
                index += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                index += rest.find("*/").map_or(rest.len(), |end| end + 2);
            } else if let Some(raw) = rest
                .strip_prefix("br")
                .or_else(|| rest.strip_prefix('r'))
                .filter(|_| {
                    index == 0
                        || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_')
                })
                .filter(|raw| raw.trim_start_matches('#').starts_with('"'))
            {
                let hashes = raw.len() - raw.trim_start_matches('#').len();
                let open = rest.len() - raw.len() + hashes + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let body = &rest[open..];
                let end = body.find(&close).unwrap_or(body.len());
                out.push(&body[..end]);
                index += open + end + close.len();
            } else if rest.starts_with('"') {
                let mut end = 1;
                while end < rest.len() && bytes[index + end] != b'"' {
                    end += if bytes[index + end] == b'\\' { 2 } else { 1 };
                }
                out.push(&rest[1..end.min(rest.len())]);
                index += end + 1;
            } else if let Some(after) = rest.strip_prefix('\'') {
                // A char literal ('x', '\'', '\u{..}'), else a lifetime.
                index += match after.find('\'') {
                    Some(end) if end < 10 && !after[..end].contains(' ') => end + 2,
                    _ => 1,
                };
            } else {
                index += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        out
    }

    /// `(on table, WHEN text, body)` of every `CREATE TRIGGER` in `sql`.
    fn triggers(sql: &str) -> Vec<(String, String, String)> {
        let idents = crate::store::replicated::sql_identifiers(sql);
        let mut out = Vec::new();
        for (index, (_, _, word)) in idents.iter().enumerate() {
            if word != "create" {
                continue;
            }
            let mut k = index + 1;
            while idents
                .get(k)
                .is_some_and(|(_, _, w)| w == "temp" || w == "temporary")
            {
                k += 1;
            }
            if idents.get(k).map(|(_, _, w)| w.as_str()) != Some("trigger") {
                continue;
            }
            let Some(on) = (k..idents.len()).find(|&i| idents[i].2 == "on") else {
                continue;
            };
            let Some((_, table_end, table)) = idents.get(on + 1) else {
                continue;
            };
            let Some(begin) = (on + 2..idents.len()).find(|&i| idents[i].2 == "begin") else {
                continue;
            };
            let mut depth = 0_i32;
            let mut end = sql.len();
            for (_, stop, word) in &idents[begin..] {
                match word.as_str() {
                    "begin" | "case" => depth += 1,
                    "end" => {
                        depth -= 1;
                        if depth == 0 {
                            end = *stop;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body_start = idents[begin].0;
            out.push((
                table.clone(),
                sql[*table_end..body_start].to_owned(),
                sql[body_start..end].to_owned(),
            ));
        }
        out
    }

    fn crate_src() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// The trigger half of the settings floor: [`SETTINGS_TRIGGER_TABLES`]
    /// names exactly the tables whose triggers write `settings` — directly,
    /// or by writing a listed table the way its trigger fires — and each
    /// discriminator is the trigger's own `WHEN` literal, never a bound value.
    #[test]
    fn settings_trigger_tables_name_every_trigger_that_writes_settings() {
        use crate::store::replicated::writes_table;
        let root = crate_src();
        let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, text) in production_sources(&root, "") {
            for sql in sql_texts(&path, &text) {
                for (table, when, body) in triggers(sql) {
                    let writes = writes_table(&body, "settings")
                        || SETTINGS_TRIGGER_TABLES.iter().any(|(listed, literal)| {
                            writes_table(&body, listed)
                                && literal.is_none_or(|literal| body.contains(literal))
                        });
                    if writes && table != "settings" {
                        found.entry(table).or_default().push(when);
                    }
                }
            }
        }
        let listed: Vec<&str> = SETTINGS_TRIGGER_TABLES
            .iter()
            .map(|(table, _)| *table)
            .collect();
        let mut tables: Vec<&str> = found.keys().map(String::as_str).collect();
        let mut expected = listed.clone();
        tables.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            tables, expected,
            "a trigger writes settings: list its table"
        );
        for (table, literal) in SETTINGS_TRIGGER_TABLES {
            if let Some(literal) = literal {
                for when in &found[*table] {
                    assert!(
                        when.contains(literal),
                        "{table}: a settings trigger fires without {literal}; arm every write"
                    );
                }
            }
        }
        // The discriminator is matched against the statement's SQL, so it
        // must always be written into the SQL, never bound as a parameter.
        let bare: Vec<&str> = SETTINGS_TRIGGER_TABLES
            .iter()
            .filter_map(|(_, literal)| literal.map(|literal| literal.trim_matches('\'')))
            .collect();
        for dir in [root.clone(), root.join("../../plurxd/src")] {
            for (path, text) in production_sources(&dir, "") {
                for literal in sql_texts(&path, &text) {
                    assert!(
                        !bare.contains(&literal),
                        "{path} binds a trigger discriminator as a value"
                    );
                }
            }
        }
    }

    /// The trigger half of the files floor (D3b): [`FILES_TRIGGER_TABLES`]
    /// names exactly the tables other than `files` whose triggers write
    /// `files` — directly, by a cascading delete, or by writing a listed
    /// table the way its trigger fires — and every such trigger's `WHEN`
    /// carries one of its table's listed literals, which the SQL always
    /// spells and never binds.
    #[test]
    fn files_trigger_tables_name_every_trigger_that_writes_files() {
        use crate::store::replicated::{deletes_rows_of, writes_table};
        let root = crate_src();
        let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, text) in production_sources(&root, "") {
            for sql in sql_texts(&path, &text) {
                for (table, when, body) in triggers(sql) {
                    let writes = writes_table(&body, "files")
                        || FILES_CASCADE_TABLES
                            .iter()
                            .any(|cascade| deletes_rows_of(&body, cascade))
                        || FILES_TRIGGER_TABLES.iter().any(|(listed, literal)| {
                            writes_table(&body, listed)
                                && literal.is_none_or(|literal| body.contains(literal))
                        });
                    if writes && table != "files" {
                        found.entry(table).or_default().push(when);
                    }
                }
            }
        }
        let mut listed: Vec<&str> = FILES_TRIGGER_TABLES
            .iter()
            .map(|(table, _)| *table)
            .collect();
        listed.sort_unstable();
        listed.dedup();
        let tables: Vec<&str> = found.keys().map(String::as_str).collect();
        assert_eq!(tables, listed, "a trigger writes files: list its table");
        for (table, whens) in &found {
            let literals: Vec<Option<&str>> = FILES_TRIGGER_TABLES
                .iter()
                .filter(|(listed, _)| listed == table)
                .map(|(_, literal)| *literal)
                .collect();
            for when in whens {
                assert!(
                    literals
                        .iter()
                        .any(|literal| literal.is_none_or(|literal| when.contains(literal))),
                    "{table}: a files trigger fires on {when:?}, which no listed literal covers"
                );
            }
        }
        let bare: Vec<&str> = FILES_TRIGGER_TABLES
            .iter()
            .filter_map(|(_, literal)| literal.map(|literal| literal.trim_matches('\'')))
            .collect();
        for dir in [root.clone(), root.join("../../plurxd/src")] {
            for (path, text) in production_sources(&dir, "") {
                for literal in sql_texts(&path, &text) {
                    assert!(
                        !bare.contains(&literal),
                        "{path} binds a files trigger discriminator as a value"
                    );
                }
            }
        }
    }

    /// `(child, parent, clause)` for every `REFERENCES` in a `CREATE TABLE`
    /// of `sql`: the referencing table, the referenced one, and the clause
    /// text up to the end of that column or constraint, lowercased.
    fn references(sql: &str) -> Vec<(String, String, String)> {
        let idents = crate::store::replicated::sql_identifiers(sql);
        let mut out = Vec::new();
        for (index, (_, _, word)) in idents.iter().enumerate() {
            if word != "create" {
                continue;
            }
            let mut k = index + 1;
            while idents
                .get(k)
                .is_some_and(|(_, _, w)| w == "temp" || w == "temporary")
            {
                k += 1;
            }
            if idents.get(k).map(|(_, _, w)| w.as_str()) != Some("table") {
                continue;
            }
            k += 1;
            if idents.get(k).map(|(_, _, w)| w.as_str()) == Some("if") {
                k += 3;
            }
            // `main.files`: the table is the identifier after the qualifier.
            while idents
                .get(k)
                .is_some_and(|(_, end, _)| sql[*end..].trim_start().starts_with('.'))
            {
                k += 1;
            }
            let Some((_, name_end, child)) = idents.get(k) else {
                continue;
            };
            let Some(open) = sql[*name_end..].find('(').map(|at| name_end + at) else {
                continue;
            };
            let mut depth = 0_i32;
            let mut close = sql.len();
            for (at, byte) in sql.bytes().enumerate().skip(open) {
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = at;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for (start, end, word) in &idents[k + 1..] {
                if *start >= close {
                    break;
                }
                if word != "references" {
                    continue;
                }
                let Some((_, parent_end, parent)) =
                    idents.iter().find(|(begin, _, _)| *begin > *end)
                else {
                    continue;
                };
                let mut depth = 0_i32;
                let mut stop = close;
                for (at, byte) in sql.bytes().enumerate().take(close).skip(*parent_end) {
                    match byte {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        b',' if depth == 0 => {
                            stop = at;
                            break;
                        }
                        _ => {}
                    }
                    if depth < 0 {
                        stop = at;
                        break;
                    }
                }
                let clause = sql[*parent_end..stop]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                out.push((child.clone(), parent.clone(), clause));
            }
        }
        out
    }

    /// The cascade half of the files floor (D3b): [`FILES_CASCADE_TABLES`]
    /// is exactly the set of tables a delete from which reaches `files`
    /// through `ON DELETE` actions, followed transitively; and no
    /// `ON UPDATE` action reaches `files`, so updates need no listing.
    #[test]
    fn files_cascade_tables_name_every_delete_that_reaches_files() {
        let root = crate_src();
        let mut edges = std::collections::BTreeSet::new();
        for (path, text) in production_sources(&root, "") {
            for sql in sql_texts(&path, &text) {
                for edge in references(sql) {
                    edges.insert(edge);
                }
            }
        }
        assert!(
            edges
                .iter()
                .any(|(child, parent, clause)| child == "files"
                    && parent == "items"
                    && clause.contains("on delete cascade")),
            "the census must see files' own reference to items: {edges:?}"
        );
        let acts = |clause: &str| {
            ["on delete cascade", "on delete set null", "on delete set default"]
                .iter()
                .any(|action| clause.contains(action))
        };
        let mut reach: std::collections::BTreeSet<String> = edges
            .iter()
            .filter(|(child, _, clause)| child == "files" && acts(clause))
            .map(|(_, parent, _)| parent.clone())
            .collect();
        loop {
            let more: Vec<String> = edges
                .iter()
                .filter(|(child, parent, clause)| {
                    reach.contains(child)
                        && clause.contains("on delete cascade")
                        && !reach.contains(parent)
                })
                .map(|(_, parent, _)| parent.clone())
                .collect();
            if more.is_empty() {
                break;
            }
            reach.extend(more);
        }
        let mut listed: Vec<&str> = FILES_CASCADE_TABLES.to_vec();
        listed.sort_unstable();
        assert_eq!(
            reach.iter().map(String::as_str).collect::<Vec<_>>(),
            listed,
            "a delete cascades into files: list its table"
        );
        for (child, parent, clause) in &edges {
            if child == "files" || reach.contains(child) {
                assert!(
                    !clause.contains("on update"),
                    "{child} -> {parent}: an ON UPDATE action reaches files; classify updates"
                );
            }
        }
    }

    /// Every production statement that may write `files` runs through the
    /// store's floored client (D3b), or writes no replicated row.
    #[test]
    fn every_files_writer_is_floored_or_writes_no_replicated_row() {
        let root = crate_src();
        let daemon = root.join("../../plurxd/src");
        let mut writers = std::collections::BTreeSet::new();
        for (crate_name, dir) in [("plurx-core", &root), ("plurxd", &daemon)] {
            for (path, text) in production_sources(dir, "") {
                if sql_texts(&path, &text).into_iter().any(writes_files) {
                    writers.insert(format!("{crate_name}/{path}"));
                }
            }
        }
        let unfloored: Vec<&String> = writers
            .iter()
            .filter(|path| {
                !(path.starts_with("plurx-core/store/")
                    // `DVR_PURGE_CATALOG`: SQL both stores run, the replicated
                    // one through TimedClient.
                    || *path == "plurx-core/dvr.rs"
                    // rusqlite on a restore image, before any node serves it.
                    || *path == "plurx-core/cluster/migration.rs"
                    // Test fixtures in `#[cfg(test)]` items the source cut
                    // keeps (it removes only a trailing `mod tests`).
                    || *path == "plurxd/http/shared_source_assets.rs"
                    || *path == "plurxd/http/shared_source_playback.rs")
            })
            .collect();
        assert!(
            unfloored.is_empty(),
            "files writers outside a floored path: {unfloored:?}"
        );
    }

    /// The other half: every production statement that may write `settings`
    /// runs through a floored writer, or writes no replicated row. A new file
    /// holding one fails here until its writer is named.
    #[test]
    fn every_settings_writer_is_floored_or_writes_no_replicated_row() {
        let root = crate_src();
        let daemon = root.join("../../plurxd/src");
        let mut writers = std::collections::BTreeSet::new();
        for (crate_name, dir) in [("plurx-core", &root), ("plurxd", &daemon)] {
            for (path, text) in production_sources(dir, "") {
                if sql_texts(&path, &text).into_iter().any(writes_settings) {
                    writers.insert(format!("{crate_name}/{path}"));
                }
            }
        }
        let unfloored: Vec<&String> = writers
            .iter()
            .filter(|path| {
                // The store: every replicated write goes through TimedClient
                // (pinned above); SQLite has no replica and no view.
                !(path.starts_with("plurx-core/store/")
                    // FlooredClient, pinned below.
                    || *path == "plurx-core/cluster/membership.rs"
                    // rusqlite on a restore image, before any node serves it.
                    || *path == "plurx-core/cluster/migration.rs"
                    // `UPDATE settings … WHERE 0`: a backup anchor, no row.
                    || *path == "plurxd/backup.rs")
            })
            .collect();
        assert!(
            unfloored.is_empty(),
            "settings writers outside a floored path: {unfloored:?}"
        );

        // The store's raw-client writes are schema installation and
        // provisioning at store construction, before the applied view
        // exists; anything else goes through TimedClient.
        for (path, text) in production_sources(&root, "store") {
            // The consistent-read census is a test-only source scanner:
            // the client calls in it are the patterns it searches for.
            if path.starts_with("store/sqlite/")
                || !path.ends_with(".rs")
                || path == "store/consistent_read_census.rs"
            {
                continue;
            }
            let mut at = 0;
            while let Some(offset) = text[at..].find("client") {
                let begin = at + offset;
                at = begin + 6;
                let after = text[at..].trim_start();
                let Some(call) = after.strip_prefix('.') else {
                    continue;
                };
                let call = call.trim_start();
                if !["execute", "txn(", "batch("]
                    .iter()
                    .any(|name| call.starts_with(name))
                {
                    continue;
                }
                if begin > 0 && {
                    let previous = text.as_bytes()[begin - 1];
                    previous.is_ascii_alphanumeric() || previous == b'_' || previous == b'.'
                } {
                    continue;
                }
                let function = text[..begin]
                    .rfind("fn ")
                    .map(|fn_at| {
                        text[fn_at + 3..]
                            .split(['<', '('])
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_owned()
                    })
                    .unwrap_or_default();
                assert!(
                    function.starts_with("bootstrap")
                        || function.starts_with("install_")
                        || function.starts_with("provision_")
                        || function == "seal_legacy",
                    "{path}: a raw-client write in `{function}` bypasses TimedClient"
                );
            }
        }

        // Membership's client is floored, and writes only through the two
        // methods that arm it.
        let membership =
            std::fs::read_to_string(root.join("cluster/membership.rs")).expect("membership source");
        let membership = &membership[..membership
            .find("\n#[cfg(test)]\nmod tests")
            .unwrap_or(membership.len())];
        assert!(membership.contains("struct ReplicatedMembership {\n    client: FlooredClient,"));
        for unarmed in [".batch(", ".execute_returning", ".execute_acked("] {
            assert!(
                !membership.contains(unarmed),
                "membership writes through `{unarmed}`, which FlooredClient does not arm"
            );
        }
    }
}
