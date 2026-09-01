//! Durable state for permanent Dolby Vision Profile 7 conversion.
//!
//! The media bytes stay on their mounted filesystem; only the state machine
//! and its audit facts replicate. A worker may therefore die after proving an
//! output without losing the proof boundary, while no database ever carries a
//! 60–80 GB intermediate through Raft.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;

use crate::error::StoreError;

pub(crate) const DV_CONVERSIONS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS dv_conversions (
    file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    state          TEXT NOT NULL,
    el_type        TEXT,
    original_path  TEXT,
    bytes_before   INTEGER,
    bytes_after    INTEGER,
    error          TEXT,
    queued_at_ms   INTEGER NOT NULL,
    finished_at_ms INTEGER,
    recovery_guard_id TEXT CHECK
                        (state != 'committed' OR original_path IS NOT NULL
                         OR recovery_guard_id IS NOT NULL)
) STRICT";

pub(crate) const DV_CONVERSIONS_QUEUE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS dv_conversions_queue
        ON dv_conversions(state, queued_at_ms, file_id)";

pub(crate) const DV_CONVERSIONS_RECOVERY_GUARD_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS dv_conversions_recovery_guard
        ON dv_conversions(recovery_guard_id) WHERE recovery_guard_id IS NOT NULL";

/// Hiqlite's `execute_returning` is the only replicated mutation API that can
/// return row values, while its multi-statement transaction API returns only
/// affected-row counts. Use one self-retiring settings row as the statement's
/// result envelope: the value is computed from the pre-admission snapshot,
/// this trigger applies the corresponding admission, and the envelope is
/// deleted before the statement commits. Nothing transient is retained or
/// imported, and the caller receives the exact facts which authorized (or
/// refused) the mutation from the same Raft log entry.
macro_rules! dv_queue_admission_trigger {
    ($if_not_exists:literal) => {
        concat!(
            "CREATE TRIGGER ",
            $if_not_exists,
            "dv_queue_admission_settings_ai
     AFTER INSERT ON settings
     WHEN NEW.key GLOB '__plurx_internal.dv_queue_admission.*' BEGIN
       INSERT INTO dv_conversions (file_id, state, queued_at_ms)
       SELECT f.id,
              'queued',
              CAST(json_extract(NEW.value, '$.requested_queued_at_ms') AS INTEGER)
         FROM files f
         JOIN items i ON i.id = f.item_id
         JOIN settings mode_setting ON mode_setting.key = 'library.dv_disk_convert'
        WHERE f.id = CAST(json_extract(NEW.value, '$.requested_file_id') AS INTEGER)
          AND json_extract(NEW.value, '$.request_kind') = 'single'
          AND json_extract(NEW.value, '$.outcome') = 'queued'
          AND LOWER(f.container) = 'mkv' AND f.dv_profile = 7
          AND f.dv_bl_compat_id IN (1, 6)
          AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
          AND json_valid(mode_setting.value)
          AND json_type(mode_setting.value) = 'object'
          AND json_extract(
                mode_setting.value, '$.\"' || i.library_id || '\"')
              IN ('manual', 'auto')
       ON CONFLICT(file_id) DO UPDATE SET
         state = 'queued', el_type = NULL, original_path = NULL,
         bytes_before = NULL, bytes_after = NULL, error = NULL,
         queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL,
         recovery_guard_id = NULL
       WHERE dv_conversions.state = 'failed';
       INSERT INTO dv_conversions (file_id, state, queued_at_ms)
       SELECT f.id,
              'queued',
              CAST(json_extract(NEW.value, '$.requested_queued_at_ms') AS INTEGER)
         FROM json_each(NEW.value, '$.candidate_ids') candidate
         JOIN files f ON f.id = CAST(candidate.value AS INTEGER)
         JOIN items i ON i.id = f.item_id
         JOIN settings mode_setting ON mode_setting.key = 'library.dv_disk_convert'
    LEFT JOIN dv_conversions d ON d.file_id = f.id
        WHERE json_extract(NEW.value, '$.request_kind') = 'library_batch'
          AND json_extract(NEW.value, '$.outcome') = 'queued'
          AND i.library_id =
                CAST(json_extract(NEW.value, '$.requested_library_id') AS INTEGER)
          AND LOWER(f.container) = 'mkv' AND f.dv_profile = 7
          AND f.dv_bl_compat_id IN (1, 6)
          AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
          AND json_valid(mode_setting.value)
          AND json_type(mode_setting.value) = 'object'
          AND json_extract(
                mode_setting.value, '$.\"' || i.library_id || '\"')
              IN ('manual', 'auto')
          AND (d.file_id IS NULL OR
               (json_extract(NEW.value, '$.retry_failed') = 1
                AND d.state = 'failed'))
       ON CONFLICT(file_id) DO UPDATE SET
         state = 'queued', el_type = NULL, original_path = NULL,
         bytes_before = NULL, bytes_after = NULL, error = NULL,
         queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL,
         recovery_guard_id = NULL
       WHERE json_extract(NEW.value, '$.retry_failed') = 1
         AND dv_conversions.state = 'failed';
       DELETE FROM settings WHERE key = NEW.key;
     END"
        )
    };
}

pub(crate) const DV_QUEUE_ADMISSION_TRIGGER: &str = dv_queue_admission_trigger!("IF NOT EXISTS ");

/// Versioned migrations must create the trigger themselves rather than rely
/// on the fresh-cluster installer. Strict DDL makes a pre-existing trigger at
/// a v24 marker fail closed: the migration can advance to v25 only when it
/// created this exact canonical body in the same replicated transaction.
pub(crate) const DV_QUEUE_ADMISSION_MIGRATION_TRIGGER: &str = dv_queue_admission_trigger!("");

/// Permanent, non-cascading witness for a replacement that must outlive its
/// catalogue row. `guard_id`, not `file_id`, is the durable identity: SQLite
/// may reuse a deleted integer row id, and a new file must never make an old
/// recovery guard look attached again.
pub(crate) const DV_RECOVERY_GUARDS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS dv_recovery_guards (
    guard_id       TEXT PRIMARY KEY,
    file_id        INTEGER NOT NULL,
    library_id     INTEGER NOT NULL,
    source_path    TEXT NOT NULL,
    recovery_path  TEXT NOT NULL UNIQUE,
    state          TEXT NOT NULL CHECK
                     (state IN ('intent','active','guard_removed','scratch_removed')),
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT";

pub(crate) const DV_RECOVERY_GUARDS_FILE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS dv_recovery_guards_file
        ON dv_recovery_guards(file_id, guard_id)";

/// Strict DDL for the versioned replicated migration.
///
/// Bootstrap is deliberately idempotent, but a migration marker must never
/// advance across a pre-existing object whose shape this binary did not
/// create. Keeping the migration strings separate preserves both properties.
pub(crate) const DV_CONVERSIONS_MIGRATION_SCHEMA: &str = "CREATE TABLE dv_conversions (
    file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    state          TEXT NOT NULL,
    el_type        TEXT,
    original_path  TEXT,
    bytes_before   INTEGER,
    bytes_after    INTEGER,
    error          TEXT,
    queued_at_ms   INTEGER NOT NULL,
    finished_at_ms INTEGER
) STRICT";

pub(crate) const DV_CONVERSIONS_MIGRATION_QUEUE_INDEX: &str = "CREATE INDEX dv_conversions_queue
        ON dv_conversions(state, queued_at_ms, file_id)";

pub(crate) const DV_RECOVERY_GUARDS_MIGRATION_COLUMN: &str =
    "ALTER TABLE dv_conversions ADD COLUMN recovery_guard_id TEXT CHECK
         (state != 'committed' OR original_path IS NOT NULL
          OR recovery_guard_id IS NOT NULL)";

pub(crate) const DV_RECOVERY_GUARDS_MIGRATION_LINK_INDEX: &str =
    "CREATE UNIQUE INDEX dv_conversions_recovery_guard
        ON dv_conversions(recovery_guard_id) WHERE recovery_guard_id IS NOT NULL";

pub(crate) const DV_RECOVERY_GUARDS_MIGRATION_SCHEMA: &str = "CREATE TABLE dv_recovery_guards (
    guard_id       TEXT PRIMARY KEY,
    file_id        INTEGER NOT NULL,
    library_id     INTEGER NOT NULL,
    source_path    TEXT NOT NULL,
    recovery_path  TEXT NOT NULL UNIQUE,
    state          TEXT NOT NULL CHECK
                     (state IN ('intent','active','guard_removed','scratch_removed')),
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT";

pub(crate) const DV_RECOVERY_GUARDS_MIGRATION_FILE_INDEX: &str =
    "CREATE INDEX dv_recovery_guards_file
        ON dv_recovery_guards(file_id, guard_id)";

/// One Raft/SQLite mutation may never admit more rows than this.
pub const DV_CONVERSION_QUEUE_BATCH_MAX: i64 = 64;

/// One admin projection may never expand into an unbounded SQL payload.
pub const DV_CONVERSION_LEDGER_READ_MAX: usize = 256;

/// One operator/cleanup guard scan may never expand without a hard ceiling.
pub const DV_RECOVERY_GUARD_READ_MAX: i64 = 256;

/// A new or retried conversion is admitted only while its library explicitly
/// opts into permanent media mutation. Existing active/committed rows remain
/// observable after an operator turns the mode off.
pub const DV_CONVERSION_MODE_DISABLED_REASON: &str = "library Dolby Vision conversion mode is Off";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DvConversionState {
    Queued,
    Running,
    Verified,
    Committed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DvRecoveryGuardState {
    Intent,
    Active,
    GuardRemoved,
    ScratchRemoved,
}

impl DvRecoveryGuardState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Active => "active",
            Self::GuardRemoved => "guard_removed",
            Self::ScratchRemoved => "scratch_removed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "intent" => Some(Self::Intent),
            "active" => Some(Self::Active),
            "guard_removed" => Some(Self::GuardRemoved),
            "scratch_removed" => Some(Self::ScratchRemoved),
            _ => None,
        }
    }

    pub(crate) fn permits_cleanup_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Intent | Self::Active, Self::GuardRemoved)
                | (Self::GuardRemoved, Self::ScratchRemoved)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DvRecoveryGuard {
    pub guard_id: String,
    pub file_id: i64,
    pub library_id: i64,
    pub source_path: String,
    pub recovery_path: String,
    pub state: DvRecoveryGuardState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvRecoveryGuardSummary {
    pub intent: i64,
    pub active: i64,
    pub guard_removed: i64,
    pub scratch_removed: i64,
    pub orphaned: i64,
}

/// One coherent operator projection of recovery-guard counts and a bounded
/// orphan page. Both fields come from one backend snapshot so a concurrent
/// cascade or cleanup transition cannot make the count disagree with the
/// page returned alongside it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvRecoveryGuardSnapshot {
    pub summary: DvRecoveryGuardSummary,
    pub orphans: Vec<DvRecoveryGuard>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DvConversionMode {
    #[default]
    Off,
    Manual,
    Auto,
}

impl DvConversionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

impl DvConversionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Verified => "verified",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "verified" => Some(Self::Verified),
            "committed" => Some(Self::Committed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DvConversion {
    pub file_id: i64,
    pub state: DvConversionState,
    pub el_type: Option<String>,
    pub original_path: Option<String>,
    pub bytes_before: Option<i64>,
    pub bytes_after: Option<i64>,
    pub error: Option<String>,
    pub queued_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub recovery_guard: Option<DvRecoveryGuard>,
}

/// A small candidate projection for a node-local cursor.
///
/// The worker reads the complete file row only after it owns the file lease.
/// That keeps an idle queue sweep bounded and prevents a losing node from
/// moving full probe JSON across the replicated store for work it cannot run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvConversionCandidate {
    pub file_id: i64,
    pub library_id: i64,
    pub state: DvConversionState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvConversionProgress {
    pub eligible: i64,
    pub queued: i64,
    pub running: i64,
    pub verified: i64,
    pub committed: i64,
    pub failed: i64,
}

impl DvConversionProgress {
    fn add_assign(&mut self, other: &Self) {
        self.eligible = self.eligible.saturating_add(other.eligible);
        self.queued = self.queued.saturating_add(other.queued);
        self.running = self.running.saturating_add(other.running);
        self.verified = self.verified.saturating_add(other.verified);
        self.committed = self.committed.saturating_add(other.committed);
        self.failed = self.failed.saturating_add(other.failed);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvConversionProgressSnapshot {
    pub global: DvConversionProgress,
    pub by_library: BTreeMap<i64, DvConversionProgress>,
}

impl DvConversionProgressSnapshot {
    pub(crate) fn from_libraries(by_library: BTreeMap<i64, DvConversionProgress>) -> Self {
        let mut global = DvConversionProgress::default();
        for progress in by_library.values() {
            global.add_assign(progress);
        }
        Self { global, by_library }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DvConversionQueueBatch {
    pub queued: u64,
    /// `true` means this batch hit its cap and another bounded pass may find
    /// more work. It intentionally does not promise that another pass will:
    /// a concurrent node may drain the remainder first.
    pub saturated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueDvConversionOutcome {
    Queued(DvConversion),
    AlreadyActive(DvConversion),
    AlreadyCommitted(DvConversion),
    Ineligible(&'static str),
    FileMissing,
}

#[async_trait]
pub trait DvConversionStore: Send + Sync + 'static {
    async fn dv_conversion(&self, file_id: i64) -> Result<Option<DvConversion>, StoreError>;

    async fn dv_conversions_for_files(
        &self,
        file_ids: &[i64],
    ) -> Result<Vec<DvConversion>, StoreError>;

    async fn dv_conversion_eligibility_for_files(
        &self,
        file_ids: &[i64],
    ) -> Result<BTreeMap<i64, bool>, StoreError>;

    async fn dv_conversion_eligible(&self, file_id: i64) -> Result<bool, StoreError>;

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError>;

    /// Queue one library's currently eligible files. Automatic discovery does
    /// not retry failed rows; an operator-triggered pass may do so explicitly.
    async fn queue_library_dv_conversion_batch(
        &self,
        library_id: i64,
        queued_at_ms: i64,
        retry_failed: bool,
        limit: i64,
    ) -> Result<DvConversionQueueBatch, StoreError>;

    async fn dv_conversion_candidates(
        &self,
        after_file_id: i64,
        limit: i64,
    ) -> Result<Vec<DvConversionCandidate>, StoreError>;

    /// Return the next guardless terminal conversion whose owned scratch may
    /// need crash-convergent cleanup. Guarded commits deliberately retain their
    /// scratch until the non-cascading guard lifecycle becomes orphaned. The
    /// caller wraps by retrying from zero.
    async fn dv_committed_cleanup_candidate(
        &self,
        after_file_id: i64,
    ) -> Result<Option<i64>, StoreError>;

    /// Read the guard linked to the current conversion attempt for `file_id`.
    /// An unlinked orphan with a reused integer file id is intentionally not
    /// returned by this method.
    async fn dv_recovery_guard(&self, file_id: i64) -> Result<Option<DvRecoveryGuard>, StoreError>;

    async fn dv_recovery_guard_by_id(
        &self,
        guard_id: &str,
    ) -> Result<Option<DvRecoveryGuard>, StoreError>;

    /// Return only guards whose exact durable id is no longer linked by a
    /// conversion row. The lexical cursor and hard cap bound every pass.
    async fn dv_recovery_guard_orphans(
        &self,
        after_guard_id: &str,
        limit: i64,
    ) -> Result<Vec<DvRecoveryGuard>, StoreError>;

    async fn dv_recovery_guard_summary(&self) -> Result<DvRecoveryGuardSummary, StoreError>;

    /// Return recovery-guard totals and one bounded orphan page from the same
    /// database snapshot. This is the operator-facing projection; cleanup uses
    /// the cheaper cursor-only orphan selector above.
    async fn dv_recovery_guard_snapshot(
        &self,
        after_guard_id: &str,
        limit: i64,
    ) -> Result<DvRecoveryGuardSnapshot, StoreError>;

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError>;

    async fn dv_conversion_progress_snapshot(
        &self,
    ) -> Result<DvConversionProgressSnapshot, StoreError>;

    /// Atomically update one member of the shared per-library mode document.
    /// Concurrent edits to different libraries must not overwrite each other.
    async fn set_library_dv_conversion_mode(
        &self,
        library_id: i64,
        mode: DvConversionMode,
    ) -> Result<bool, StoreError>;

    async fn begin_dv_recovery_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        recovery_path: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError>;

    async fn mark_dv_conversion_verified(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
    ) -> Result<bool, StoreError>;

    async fn mark_dv_conversion_committed(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn mark_dv_conversion_committed_with_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn advance_dv_recovery_guard(
        &self,
        guard_id: &str,
        expected: DvRecoveryGuardState,
        next: DvRecoveryGuardState,
        updated_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn delete_dv_recovery_guard(&self, guard_id: &str) -> Result<bool, StoreError>;

    async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError>;
}

pub(crate) fn validate_recovery_guard_identity(
    guard_id: &str,
    recovery_path: &str,
) -> Result<(), StoreError> {
    if guard_id.is_empty() || guard_id.len() > 128 {
        return Err(StoreError::Task(
            "Dolby Vision recovery guard id must contain 1..=128 bytes".to_owned(),
        ));
    }
    if recovery_path.is_empty() || recovery_path.len() > 4096 {
        return Err(StoreError::Task(
            "Dolby Vision recovery path must contain 1..=4096 bytes".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn eligibility_reason(
    container: Option<&str>,
    profile: Option<i64>,
    bl_compat_id: Option<i64>,
    el_present: Option<bool>,
    rpu_present: Option<bool>,
) -> Option<&'static str> {
    if !container.is_some_and(|value| value.eq_ignore_ascii_case("mkv")) {
        return Some("file is not a Matroska MKV");
    }
    if profile != Some(7) {
        return Some("file is not Dolby Vision Profile 7");
    }
    if !matches!(bl_compat_id, Some(1 | 6)) {
        return Some("Profile 7 base layer is not HDR10-compatible");
    }
    if el_present != Some(true) {
        return Some("Profile 7 enhancement layer is not present");
    }
    if rpu_present != Some(true) {
        return Some("Profile 7 RPU is not present");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_queue_trigger_is_the_strict_canonical_shape() {
        assert!(!DV_QUEUE_ADMISSION_MIGRATION_TRIGGER.contains("IF NOT EXISTS"));
        assert_eq!(
            DV_QUEUE_ADMISSION_MIGRATION_TRIGGER.replacen(
                "CREATE TRIGGER ",
                "CREATE TRIGGER IF NOT EXISTS ",
                1
            ),
            DV_QUEUE_ADMISSION_TRIGGER
        );
    }

    #[test]
    fn disk_conversion_requires_the_numeric_hdr10_compatible_profile_7_shape() {
        assert_eq!(
            eligibility_reason(Some("mkv"), Some(7), Some(1), Some(true), Some(true)),
            None
        );
        assert_eq!(
            eligibility_reason(Some("MKV"), Some(7), Some(6), Some(true), Some(true)),
            None
        );
        assert!(
            eligibility_reason(Some("mp4"), Some(7), Some(1), Some(true), Some(true)).is_some()
        );
        assert!(
            eligibility_reason(Some("mkv"), Some(7), Some(4), Some(true), Some(true)).is_some()
        );
        assert!(
            eligibility_reason(Some("mkv"), Some(8), Some(1), Some(false), Some(true)).is_some()
        );
        assert!(eligibility_reason(Some("mkv"), Some(7), Some(1), Some(true), None).is_some());
    }
}
