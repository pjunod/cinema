//! Durable state for permanent Dolby Vision Profile 7 conversion.
//!
//! The media bytes stay on their mounted filesystem; only the state machine
//! and its audit facts replicate. A worker may therefore die after proving an
//! output without losing the proof boundary, while no database ever carries a
//! 60–80 GB intermediate through Raft.

use async_trait::async_trait;
use serde::Serialize;

use crate::error::StoreError;

pub(crate) const DV_CONVERSIONS_SCHEMA: &str = "CREATE TABLE dv_conversions (
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

pub(crate) const DV_CONVERSIONS_QUEUE_INDEX: &str = "CREATE INDEX dv_conversions_queue
        ON dv_conversions(state, queued_at_ms, file_id)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DvConversionState {
    Queued,
    Running,
    Verified,
    Committed,
    Failed,
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

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError>;

    /// Queue one library's currently eligible files. Automatic discovery does
    /// not retry failed rows; an operator-triggered pass may do so explicitly.
    async fn queue_library_dv_conversions(
        &self,
        library_id: i64,
        queued_at_ms: i64,
        retry_failed: bool,
    ) -> Result<u64, StoreError>;

    async fn dv_conversion_candidates(
        &self,
        after_file_id: i64,
        limit: i64,
    ) -> Result<Vec<DvConversionCandidate>, StoreError>;

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError>;

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

    async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError>;
}

pub(crate) fn eligibility_reason(
    profile: Option<i64>,
    bl_compat_id: Option<i64>,
    el_present: Option<bool>,
    rpu_present: Option<bool>,
) -> Option<&'static str> {
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
    fn disk_conversion_requires_the_numeric_hdr10_compatible_profile_7_shape() {
        assert_eq!(
            eligibility_reason(Some(7), Some(1), Some(true), Some(true)),
            None
        );
        assert_eq!(
            eligibility_reason(Some(7), Some(6), Some(true), Some(true)),
            None
        );
        assert!(eligibility_reason(Some(7), Some(4), Some(true), Some(true)).is_some());
        assert!(eligibility_reason(Some(8), Some(1), Some(false), Some(true)).is_some());
        assert!(eligibility_reason(Some(7), Some(1), Some(true), None).is_some());
    }
}
