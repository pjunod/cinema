//! The create's one planning read (playback-start D3b,
//! `docs/streaming/PLAYBACK-START-D3-BUILD.md` §4.2–4.4).
//!
//! A create reads its planning inputs once — the file row, its probe, every
//! key of [`crate::transcode::QUALITY_PLANNING_KEYS`] and the planning
//! generation, in one statement — as a [`StartPlan`] from the node's applied
//! view behind the settings and files floors, and hands that plan down every
//! chain that used to read them again. A helper on such a chain takes
//! `plan: Option<&StartPlan>`: `Some` answers from the plan, `None` reads the
//! Store exactly as before D3b. Whether a call may use plan values is a
//! property of the call, not of the function: only the ingress create
//! (`create_with_purpose_inner`) and the remote owner's start
//! (`internal_media_sessions::start`) ever pass `Some`, and a source test
//! pins that.
//!
//! **The binding.** Since D3t every planning key moves
//! `playback_input_generation`, so two plans with one [`PlanningBinding`]
//! hold the same values. A remote owner plans again from its own view at the
//! binding the ingress sent ([`owner_plan`]): equal, it builds at that
//! revision; different, it asks Authority once, and a still-different answer
//! refuses the start (`409`, the answer it already gave for a changed
//! source). The refusal closes the ingress's view ([`close_after_refusal`]):
//! the ingress may be the stale side, so its next plan is a quorum read.
//! No wire change: the binding was already on the start request.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use plurx_core::domain::MediaFile;
use plurx_core::error::StoreError;
use plurx_core::store::{
    AppliedTable, AppliedView, PlaybackPlanningSnapshot, ViewFallback, ViewSource,
};

use crate::media_pool::PlanningBinding;

/// One create's planning inputs, read once.
#[derive(Debug)]
pub(crate) struct StartPlan {
    /// The file, its probe, the planning keys, the generation and the title.
    pub(crate) snapshot: Arc<PlaybackPlanningSnapshot>,
    /// The binding a peer checks its own plan against (unchanged wire type).
    pub(crate) binding: PlanningBinding,
    /// Where the snapshot came from: the replica, or Authority.
    pub(crate) source: ViewSource,
}

impl StartPlan {
    pub(crate) fn new(snapshot: PlaybackPlanningSnapshot, source: ViewSource) -> Self {
        let binding = PlanningBinding::from_snapshot(&snapshot);
        Self {
            snapshot: Arc::new(snapshot),
            binding,
            source,
        }
    }

    /// One planning setting as stored; `None` when unset.
    pub(crate) fn setting(&self, key: &str) -> Option<&str> {
        self.snapshot.settings.get(key).map(String::as_str)
    }

    /// A boolean planning setting with the store's own parsing and default.
    pub(crate) fn switch(&self, key: &str, default: bool) -> bool {
        plurx_core::store::stored_switch(self.setting(key), default)
    }

    /// Whether the plan describes exactly this file revision: id, size and
    /// modification time.
    pub(crate) fn describes(&self, file: &MediaFile) -> bool {
        let planned = &self.snapshot.file;
        planned.id == file.id && planned.size == file.size && planned.mtime == file.mtime
    }

    /// The stored probe the plan holds for `file`, when it describes exactly
    /// that revision; `None` asks the caller to read it.
    pub(crate) fn probe_for(&self, file: &MediaFile) -> Option<Option<&str>> {
        self.describes(file)
            .then(|| self.snapshot.probe_json.as_deref())
    }
}

/// The ingress create's plan: one read through the applied view (§4.2).
pub(crate) async fn ingress_plan(
    applied: &AppliedView,
    file_id: i64,
) -> Result<Option<StartPlan>, StoreError> {
    Ok(applied
        .planning(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await?
        .map(|(snapshot, source)| StartPlan::new(snapshot, source)))
}

/// Why a remote owner refused to plan at the ingress's binding.
#[derive(Debug)]
pub(crate) enum OwnerPlanError {
    /// The Store could not answer.
    Store(StoreError),
    /// Authority itself holds another revision of the source or settings.
    Changed,
}

impl From<StoreError> for OwnerPlanError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// A remote owner's plan (§4.3). With the ingress's binding: one view read,
/// and Authority once only when the view answers another binding. Without
/// one (a legacy route): Authority, once, for the whole start. `Ok(None)`
/// when the source is gone; the chain then reads and refuses as before.
pub(crate) async fn owner_plan(
    applied: &AppliedView,
    file_id: i64,
    expected: Option<&PlanningBinding>,
) -> Result<Option<StartPlan>, OwnerPlanError> {
    let keys = &crate::transcode::QUALITY_PLANNING_KEYS;
    let Some(expected) = expected else {
        return Ok(applied
            .planning_from_authority(file_id, keys, ViewFallback::Unbound)
            .await?
            .map(|(snapshot, source)| StartPlan::new(snapshot, source)));
    };
    if let Some((snapshot, source)) = applied.planning(file_id, keys).await? {
        let plan = StartPlan::new(snapshot, source);
        if plan.binding == *expected {
            return Ok(Some(plan));
        }
        if source == ViewSource::Authority {
            return Err(OwnerPlanError::Changed);
        }
    }
    match applied
        .planning_from_authority(file_id, keys, ViewFallback::BindingMismatch)
        .await?
    {
        Some((snapshot, source)) => {
            let plan = StartPlan::new(snapshot, source);
            if plan.binding == *expected {
                Ok(Some(plan))
            } else {
                Err(OwnerPlanError::Changed)
            }
        }
        None => Ok(None),
    }
}

/// Views closed because an owner refused this node's plan.
static REFUSAL_CLOSES: AtomicU64 = AtomicU64::new(0);

/// An owner refused a start planned here as changed (§4.3): this node may be
/// the stale side. A quorum fact closes the view — both planning floors —
/// so the next plan here is Authority's until the replica is proven to
/// contain what the owner saw. No retry, no loop: the create answers the
/// typed `quality_catalog_unavailable` and the viewer's retry re-plans.
pub(crate) fn close_after_refusal(applied: &AppliedView) {
    applied.note_fact(AppliedTable::Settings);
    applied.note_fact(AppliedTable::Files);
    REFUSAL_CLOSES.fetch_add(1, Ordering::Relaxed);
}

/// [`close_after_refusal`]'s count since start, for Settings → Developer.
pub(crate) fn refusal_closes() -> u64 {
    REFUSAL_CLOSES.load(Ordering::Relaxed)
}

#[cfg(test)]
#[path = "start_plan_tests.rs"]
mod tests;
